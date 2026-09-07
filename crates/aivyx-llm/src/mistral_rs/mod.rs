//! Embedded Rust-native inference via the `mistralrs` crate, ported
//! from aivyx's own Phase 134 -- with real token streaming from day
//! one (see `provider.rs`'s module doc for why aivyx's own version
//! shipped non-streaming and what this version does differently).

pub mod convert;
pub mod provider;

pub use provider::MistralRsBackend;
