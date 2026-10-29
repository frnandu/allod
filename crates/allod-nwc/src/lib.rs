//! Nostr Wallet Connect service (SPEC §8): relays, the request pipeline, info events and
//! the NIP-47 method handlers.

pub mod contacts;
pub mod error;
pub mod handlers;
mod service;

pub use error::{nwc_error_from, Code, NwcError};
pub use service::NwcService;
