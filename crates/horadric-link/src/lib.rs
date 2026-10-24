//! What a Horadric on Linux needs that Windows has built in: randomness,
//! AES-GCM and ECDH P-256 (CNG there), and TLS, a WebSocket and HTTPS
//! (WinHTTP there). Each speaks byte for byte what the Windows side
//! speaks, so a node and a PC can seal for each other and agree on keys;
//! a test in `horadric-ui` runs both over the same keys and frames.
//!
//! Everything blocks, like the rest of Horadric's threads: callers run it
//! on a thread of their own.

pub mod crypto;
pub mod ecdh;
pub mod http;
mod stream;
pub mod ws;

pub use crypto::{gcm, open, open_bound, random, seal, seal_bound};
