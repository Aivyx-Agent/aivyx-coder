//! `MistralRsBackend` -- aivyx-coder's `LlmBackend` impl over an
//! in-process `mistralrs` model, with real token streaming (unlike
//! aivyx's own Phase 134 MistralRsProvider, which shipped
//! non-streaming and never followed up).
//!
//! ## The streaming shim
//!
//! mistral.rs's `Model::stream_chat_request` returns a `Stream<'_>`
//! borrowed from the model, which can't satisfy this trait's
//! `BoxStream<'static, ...>` requirement directly. The fix: drive the
//! borrowed stream entirely inside a spawned task holding its own
//! `Arc<Model>` clone (the borrow is satisfied within that task's own
//! scope), forwarding converted, fully-owned events out through an
//! `mpsc` channel wrapped as `ReceiverStream` -- which is genuinely
//! `'static` since it borrows nothing.
//!
//! `forward_stream_via_mpsc` is deliberately generic over the source
//! stream's item type, not tied to mistral.rs's own `Stream<'_>` --
//! this is what makes the forwarding *logic itself* (ordering,
//! stop-on-receiver-drop, error propagation) testable with a fake
//! stream, in an environment with no real GGUF model or GPU.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{Stream, StreamExt};
use mistralrs::{GgufModelBuilder, Model};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatRequest, LlmBackend, LlmError, StreamEvent};
use crate::mistral_rs::convert::{append_message_to_builder, apply_tools};

/// Drives `stream` to completion, forwarding each item (after
/// conversion via `convert`) through `tx`. Stops early if the receiver
/// has been dropped (`tx.send` fails) -- the caller lost interest, so
/// continuing to drive inference would waste CPU/GPU on a turn nobody
/// is reading anymore. Generic over the source item type `T` so this
/// can be tested with a fake stream instead of a real mistral.rs one.
async fn forward_stream_via_mpsc<S, T>(
    mut stream: S,
    tx: mpsc::Sender<Result<StreamEvent, LlmError>>,
    convert: impl Fn(T) -> Vec<Result<StreamEvent, LlmError>>,
) where
    S: Stream<Item = T> + Unpin,
{
    while let Some(item) = stream.next().await {
        for event in convert(item) {
            if tx.send(event).await.is_err() {
                return;
            }
        }
    }
}

/// In-process LLM backend over a loaded mistral.rs model. The model is
/// loaded once at construction and reused across every `stream_chat`
/// call via a cheap `Arc` clone.
pub struct MistralRsBackend {
    model: Arc<Model>,
    model_id: String,
    #[allow(dead_code)] // wired into request construction once tool-call
    // constraining lands; kept as a field now so the constructor
    // signature (and Task 5's call site) doesn't need to change later.
    constrain_tool_calls: bool,
}

impl MistralRsBackend {
    /// Loads the configured GGUF model. Async because mistral.rs's
    /// builder performs the load (mmap + tokenizer init + chat template
    /// parse) itself.
    ///
    /// Deliberately never calls `.with_tool_callback(...)` or
    /// `.with_search(...)`/`.with_search_callback(...)` on the builder
    /// below -- mistral.rs supports in-engine tool execution, but
    /// registering a callback here would let the inference engine
    /// execute tools directly, entirely bypassing this repo's own
    /// ConfirmationGate/permission-gate path every other tool call
    /// goes through. Tool calls from this backend must only ever flow
    /// out as `StreamEvent::ToolCallComplete` proposals for the
    /// existing turn loop to gate exactly like every other backend.
    pub async fn new(
        model_path: PathBuf,
        model_file: Option<String>,
        chat_template_path: Option<PathBuf>,
        constrain_tool_calls: bool,
        model_id: String,
    ) -> Result<Self, LlmError> {
        let (dir, files) = match &model_file {
            Some(f) => (model_path.to_string_lossy().to_string(), vec![f.clone()]),
            None => {
                let parent = model_path
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| ".".to_string());
                let filename = model_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .ok_or_else(|| {
                        LlmError::Parse(format!(
                            "mistralrs: cannot extract filename from {model_path:?}"
                        ))
                    })?;
                (parent, vec![filename])
            }
        };

        let full_path = std::path::Path::new(&dir).join(&files[0]);
        if !full_path.exists() {
            return Err(LlmError::Parse(format!(
                "mistralrs: model file not found at {full_path:?} -- \
                 mistralrs_model_path must be an absolute path to a real, \
                 already-downloaded local GGUF file or directory (this repo \
                 never auto-downloads models). If this path looks right but \
                 still fails, check for a typo or a relative path -- \
                 mistralrs_model_path is not `~`-expanded."
            )));
        }

        let mut builder = GgufModelBuilder::new(dir, files);
        if let Some(template) = &chat_template_path {
            builder = builder.with_chat_template(template.to_string_lossy().to_string());
        }
        let model = builder
            .build()
            .await
            .map_err(|e| LlmError::Parse(format!("mistralrs: model load failed: {e}")))?;

        Ok(MistralRsBackend {
            model: Arc::new(model),
            model_id,
            constrain_tool_calls,
        })
    }
}

/// Converts one mistral.rs `Response` item into zero or more of
/// aivyx-coder's own `StreamEvent`s. A single `Response::Chunk` can
/// carry text, reasoning text, tool calls, and/or a finish reason all
/// at once (confirmed against the real vendored `mistralrs-core-0.8.1`
/// source: `ChunkChoice { finish_reason: Option<String>, delta: Delta,
/// .. }`, `Delta { content, tool_calls, reasoning_content, .. }`), so
/// this returns a `Vec` rather than a single event -- the caller sends
/// each returned item through the channel in order.
///
/// Tool calls arrive fully-formed in one delta (confirmed against
/// `pipeline/sampling.rs`: mistral.rs finalizes and sends the complete
/// `ToolCallResponse` -- full name, full arguments string -- on the
/// same delta that sets `finish_reason`, never as incremental argument
/// fragments), so no cross-chunk accumulation is needed here.
fn convert_response_to_stream_event(response: mistralrs::Response) -> Vec<Result<StreamEvent, LlmError>> {
    match response {
        mistralrs::Response::Chunk(chunk) => {
            let mut events = Vec::new();
            let Some(choice) = chunk.choices.first() else {
                return vec![Ok(StreamEvent::Done { finish_reason: crate::backend::FinishReason::Stop })];
            };

            if let Some(text) = &choice.delta.content
                && !text.is_empty()
            {
                events.push(Ok(StreamEvent::TextDelta(text.clone())));
            }
            if let Some(reasoning) = &choice.delta.reasoning_content
                && !reasoning.is_empty()
            {
                events.push(Ok(StreamEvent::ReasoningDelta(reasoning.clone())));
            }
            if let Some(calls) = &choice.delta.tool_calls {
                for tc in calls {
                    let arguments = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::json!({}));
                    events.push(Ok(StreamEvent::ToolCallComplete(aivyx_types::ToolCall {
                        id: aivyx_types::ToolCallId(tc.id.clone()),
                        name: tc.function.name.clone(),
                        arguments,
                        source: aivyx_types::ToolCallSource::Native,
                    })));
                }
            }
            if let Some(reason_str) = &choice.finish_reason {
                let finish_reason = match reason_str.as_str() {
                    "stop" => crate::backend::FinishReason::Stop,
                    "tool_calls" => crate::backend::FinishReason::ToolCalls,
                    "length" => crate::backend::FinishReason::Length,
                    // "canceled" / "generated_image" / "generated_speech" / anything
                    // unrecognized -- Stop is the safe default; Error is reserved
                    // for genuine Err(..) results, not an unusual-but-successful stop.
                    _ => crate::backend::FinishReason::Stop,
                };
                events.push(Ok(StreamEvent::Done { finish_reason }));
                if let Some(usage) = &chunk.usage {
                    events.push(Ok(StreamEvent::Usage {
                        prompt_tokens: usage.prompt_tokens as u32,
                        completion_tokens: usage.completion_tokens as u32,
                    }));
                }
            }
            events
        }
        mistralrs::Response::Done(_done) => vec![Ok(StreamEvent::Done {
            finish_reason: crate::backend::FinishReason::Stop,
        })],
        // `mistralrs::Response` does not implement `Debug` (confirmed
        // against the real source -- unlike its sibling `ResponseOk`),
        // so the diagnostic below names the variant by hand rather than
        // via `{:?}`.
        other => {
            let kind = match other {
                mistralrs::Response::InternalError(_) => "InternalError",
                mistralrs::Response::ValidationError(_) => "ValidationError",
                mistralrs::Response::ModelError(_, _) => "ModelError",
                mistralrs::Response::CompletionModelError(_, _) => "CompletionModelError",
                mistralrs::Response::CompletionDone(_) => "CompletionDone",
                mistralrs::Response::CompletionChunk(_) => "CompletionChunk",
                mistralrs::Response::ImageGeneration(_) => "ImageGeneration",
                mistralrs::Response::Speech { .. } => "Speech",
                mistralrs::Response::Raw { .. } => "Raw",
                mistralrs::Response::Embeddings { .. } => "Embeddings",
                mistralrs::Response::Chunk(_) | mistralrs::Response::Done(_) => unreachable!(),
            };
            vec![Err(LlmError::Parse(format!(
                "mistralrs: unexpected response variant in chat stream: {kind}"
            )))]
        }
    }
}

#[async_trait]
impl LlmBackend for MistralRsBackend {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    async fn stream_chat(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        let model = Arc::clone(&self.model);
        let mut builder = mistralrs::RequestBuilder::new();
        for msg in &request.messages {
            builder = append_message_to_builder(builder, msg);
        }
        builder = apply_tools(builder, &request.tools)
            .map_err(|e| LlmError::Parse(format!("mistralrs tool conversion: {e}")))?;
        if let Some(temp) = request.temperature {
            builder = builder.set_sampler_temperature(temp.into());
        }
        if let Some(max_tokens) = request.max_tokens {
            builder = builder.set_sampler_max_len(max_tokens as usize);
        }

        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(async move {
            match model.stream_chat_request(builder).await {
                Ok(stream) => {
                    forward_stream_via_mpsc(stream, tx, convert_response_to_stream_event).await;
                }
                Err(e) => {
                    let _ = tx
                        .send(Err(LlmError::Parse(format!(
                            "mistralrs stream_chat_request: {e}"
                        ))))
                        .await;
                }
            }
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }
}

#[cfg(test)]
mod construction_tests {
    use super::*;

    #[tokio::test]
    async fn new_fails_clearly_for_a_nonexistent_model_path() {
        let result = MistralRsBackend::new(
            std::path::PathBuf::from("/definitely/does/not/exist/model.gguf"),
            None,
            None,
            false,
            "test-model".to_string(),
        )
        .await;
        let err = match result {
            Ok(_) => panic!("must fail for a nonexistent path"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("not found"),
            "error should say the file wasn't found, got: {err}"
        );
    }
}

#[cfg(test)]
mod forwarding_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn forwards_items_in_order() {
        let (tx, rx) = mpsc::channel(8);
        let source = futures::stream::iter(vec![1, 2, 3]);
        forward_stream_via_mpsc(source, tx, |n: i32| {
            vec![Ok(StreamEvent::TextDelta(n.to_string()))]
        })
        .await;

        let received: Vec<_> = ReceiverStream::new(rx).collect().await;
        assert_eq!(received.len(), 3);
        for (i, event) in received.into_iter().enumerate() {
            match event.expect("ok") {
                StreamEvent::TextDelta(s) => assert_eq!(s, (i as i32 + 1).to_string()),
                other => panic!("expected TextDelta, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn forwards_a_conversion_error_rather_than_panicking_or_dropping_it() {
        let (tx, rx) = mpsc::channel(8);
        let source = futures::stream::iter(vec![1, 2]);
        forward_stream_via_mpsc(source, tx, |n: i32| {
            if n == 2 {
                vec![Err(LlmError::Parse("boom".to_string()))]
            } else {
                vec![Ok(StreamEvent::TextDelta(n.to_string()))]
            }
        })
        .await;

        let received: Vec<_> = ReceiverStream::new(rx).collect().await;
        assert_eq!(received.len(), 2);
        assert!(received[0].is_ok());
        assert!(matches!(received[1], Err(LlmError::Parse(_))));
    }

    #[tokio::test]
    async fn stops_driving_the_source_once_the_receiver_is_dropped_mid_stream() {
        // A stream that counts how many times it's been polled, and
        // never ends -- if `forward_stream_via_mpsc` doesn't stop once
        // the receiver is dropped, this task would spin forever (the
        // test's own timeout below is what actually catches that).
        struct CountingInfiniteStream {
            polls: Arc<AtomicUsize>,
        }
        impl Stream for CountingInfiniteStream {
            type Item = i32;
            fn poll_next(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<i32>> {
                self.polls.fetch_add(1, Ordering::SeqCst);
                std::task::Poll::Ready(Some(0))
            }
        }

        let polls = Arc::new(AtomicUsize::new(0));
        // Unbuffered-ish: a small bounded channel so the producer task
        // genuinely blocks on `tx.send` waiting for items to be read,
        // keeping it alive and running concurrently with this test
        // rather than racing to completion before we can drop `rx`.
        let (tx, mut rx) = mpsc::channel(1);
        let source = CountingInfiniteStream { polls: Arc::clone(&polls) };

        let forwarding_task = tokio::spawn(async move {
            forward_stream_via_mpsc(source, tx, |n: i32| vec![Ok(StreamEvent::TextDelta(n.to_string()))])
                .await;
        });

        // Read exactly 2 real items while the forwarding task is
        // genuinely running concurrently in the background.
        for _ in 0..2 {
            let item = rx.recv().await.expect("forwarding task must still be alive");
            assert!(matches!(item, Ok(StreamEvent::TextDelta(_))));
        }

        // Now drop the receiver *while the producer is still running*
        // (it's blocked on the next `tx.send`, waiting for a reader).
        drop(rx);

        // The forwarding task must notice the closed channel and finish
        // promptly -- bound the wait so a real regression (spinning
        // forever) fails this test instead of hanging the suite.
        tokio::time::timeout(std::time::Duration::from_secs(5), forwarding_task)
            .await
            .expect("forwarding task must stop shortly after the receiver is dropped mid-stream")
            .expect("forwarding task must not panic");

        assert!(
            polls.load(Ordering::SeqCst) <= 4,
            "expected the forwarding loop to stop shortly after the receiver was \
             dropped mid-stream, but the source was polled {} times",
            polls.load(Ordering::SeqCst)
        );
    }
}

#[cfg(test)]
mod conversion_tests {
    use super::*;

    #[test]
    fn convert_chunk_with_tool_call_delta_emits_tool_call_complete() {
        let chunk = mistralrs::ChatCompletionChunkResponse {
            id: "1".to_string(),
            choices: vec![mistralrs::ChunkChoice {
                finish_reason: Some("tool_calls".to_string()),
                index: 0,
                delta: mistralrs::Delta {
                    content: None,
                    role: "assistant".to_string(),
                    tool_calls: Some(vec![mistralrs::ToolCallResponse {
                        index: 0,
                        id: "call_abc".to_string(),
                        tp: mistralrs::ToolCallType::Function,
                        function: mistralrs::CalledFunction {
                            name: "fs.read".to_string(),
                            arguments: r#"{"path":"/etc/hosts"}"#.to_string(),
                        },
                    }]),
                    reasoning_content: None,
                },
                logprobs: None,
            }],
            created: 0,
            model: "test".to_string(),
            system_fingerprint: String::new(),
            object: "chat.completion.chunk".to_string(),
            usage: None,
        };
        let events = convert_response_to_stream_event(mistralrs::Response::Chunk(chunk));
        let tool_call_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, Ok(StreamEvent::ToolCallComplete(_))))
            .collect();
        assert_eq!(tool_call_events.len(), 1, "expected exactly one ToolCallComplete event, got {events:?}");
        if let Ok(StreamEvent::ToolCallComplete(call)) = &events[0] {
            assert_eq!(call.name, "fs.read");
            assert_eq!(call.arguments, serde_json::json!({"path": "/etc/hosts"}));
        } else {
            panic!("expected the first event to be ToolCallComplete, got {:?}", events[0]);
        }
        // A finish_reason of "tool_calls" must also produce a Done event.
        assert!(events.iter().any(|e| matches!(e, Ok(StreamEvent::Done { finish_reason: crate::backend::FinishReason::ToolCalls }))));
    }

    #[test]
    fn convert_chunk_with_only_text_emits_one_text_delta_and_no_tool_call() {
        let chunk = mistralrs::ChatCompletionChunkResponse {
            id: "1".to_string(),
            choices: vec![mistralrs::ChunkChoice {
                finish_reason: None,
                index: 0,
                delta: mistralrs::Delta {
                    content: Some("hello".to_string()),
                    role: "assistant".to_string(),
                    tool_calls: None,
                    reasoning_content: None,
                },
                logprobs: None,
            }],
            created: 0,
            model: "test".to_string(),
            system_fingerprint: String::new(),
            object: "chat.completion.chunk".to_string(),
            usage: None,
        };
        let events = convert_response_to_stream_event(mistralrs::Response::Chunk(chunk));
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(StreamEvent::TextDelta(t)) if t == "hello"));
    }
}
