//! Adapts `aivyx-llm`'s `LlmBackend` (the same instance the agent's own
//! turn loop uses — no separate, duplicate LLM connection) to
//! `aivyx-vision-svg`'s `TextCompleter` seam.

use std::sync::Arc;

use aivyx_llm::backend::{ChatRequest, LlmBackend, StreamEvent, ToolChoice};
use aivyx_types::{Message, Role};
use aivyx_vision_svg::{TextCompleter, TextCompleterError};
use futures::StreamExt;

pub struct CoderTextCompleter {
    backend: Arc<dyn LlmBackend>,
    max_tokens: u32,
}

impl CoderTextCompleter {
    pub fn new(backend: Arc<dyn LlmBackend>, max_tokens: u32) -> Self {
        Self {
            backend,
            max_tokens,
        }
    }
}

#[async_trait::async_trait]
impl TextCompleter for CoderTextCompleter {
    async fn complete(&self, prompt: &str) -> Result<String, TextCompleterError> {
        let request = ChatRequest {
            messages: vec![Message::text(Role::User, prompt)],
            tools: vec![],
            tool_choice: ToolChoice::None,
            temperature: None,
            max_tokens: Some(self.max_tokens),
            id_slot: None,
            slot_hint: None,
        };
        let mut stream = self
            .backend
            .stream_chat(request)
            .await
            .map_err(|e| TextCompleterError(e.to_string()))?;

        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let StreamEvent::TextDelta(chunk) =
                event.map_err(|e| TextCompleterError(e.to_string()))?
            {
                text.push_str(&chunk);
            }
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_llm::backend::LlmError;
    use aivyx_types::ContentBlock;
    use futures::stream;
    use std::sync::Mutex;

    /// A fake `LlmBackend` streaming a canned sequence of text chunks and
    /// recording the prompt it was sent — mirrors `aivyx-pa`'s own
    /// `LlmTextCompleter` test double, adapted to this trait's
    /// `BoxStream`-returning shape instead of a `next_event`/`finish`
    /// pair. `captured_prompt` uses `std::sync::Mutex`, not `tokio::sync`
    /// — the lock is held only long enough for a synchronous write/read,
    /// never across an `.await`, so the sync primitive is correct and
    /// simpler.
    struct FakeBackend {
        chunks: Vec<String>,
        captured_prompt: Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl LlmBackend for FakeBackend {
        fn model_id(&self) -> &str {
            "fake-model"
        }

        async fn stream_chat(
            &self,
            request: ChatRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError>
        {
            let prompt = request
                .messages
                .first()
                .and_then(|m| m.content.first())
                .and_then(|block| match block {
                    ContentBlock::Text(s) => Some(s.clone()),
                    _ => None,
                });
            *self.captured_prompt.lock().unwrap() = prompt;
            let events: Vec<Result<StreamEvent, LlmError>> = self
                .chunks
                .iter()
                .cloned()
                .map(|c| Ok(StreamEvent::TextDelta(c)))
                .collect();
            Ok(stream::iter(events).boxed())
        }
    }

    #[tokio::test]
    async fn complete_concatenates_multiple_streamed_chunks_in_order() {
        let backend = Arc::new(FakeBackend {
            chunks: vec![
                "<svg xmlns=\"".to_string(),
                "http://www.w3.org/2000/svg\">".to_string(),
                "<circle r=\"5\"/>".to_string(),
                "</svg>".to_string(),
            ],
            captured_prompt: Mutex::new(None),
        });
        let completer = CoderTextCompleter::new(Arc::clone(&backend) as Arc<dyn LlmBackend>, 2048);
        let result = completer.complete("draw a circle").await.unwrap();
        assert_eq!(
            result,
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"5\"/></svg>"
        );
    }

    #[tokio::test]
    async fn complete_sends_the_user_prompt_to_the_backend() {
        let backend = Arc::new(FakeBackend {
            chunks: vec!["<svg></svg>".to_string()],
            captured_prompt: Mutex::new(None),
        });
        let completer = CoderTextCompleter::new(Arc::clone(&backend) as Arc<dyn LlmBackend>, 2048);
        completer.complete("a purple hexagon icon").await.unwrap();
        assert_eq!(
            backend.captured_prompt.lock().unwrap().as_deref(),
            Some("a purple hexagon icon")
        );
    }
}
