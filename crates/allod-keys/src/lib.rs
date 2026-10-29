//! Keys and secrets (SPEC §4).
//!
//! - [`RecoveryPhrase`]: the BIP39 mnemonic. In managed mode it is also ldk-server's
//!   `keys_mnemonic`, so the user backs up one phrase (SPEC §2.3.3).
//! - [`NostrKeys`]: Nostr keys derived from the phrase with BIP32 (SPEC §4.2).
//! - [`SecretStore`]: secret files (0600) outside the database (SPEC §4.3).
//! - [`Vault`]: XChaCha20-Poly1305 sealing for backups and exports, keyed from the
//!   recovery phrase (SPEC §4.3).

mod derive;
mod phrase;
mod secrets;
mod vault;

pub use derive::{KeyPair, NostrKeys};
pub use phrase::RecoveryPhrase;
pub use secrets::{create_private_dir, names as secret_names, write_file_with_mode, SecretStore};
pub use vault::{Vault, VaultParams, ENCRYPTED_PREFIX};

/// Errors from this crate. Messages never contain secret material.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    /// The recovery phrase is not a valid 12- or 24-word BIP39 English mnemonic.
    #[error("invalid recovery phrase: {0}")]
    InvalidPhrase(String),
    /// Key derivation failed (should not happen for valid input).
    #[error("key derivation failed: {0}")]
    Derivation(String),
    /// App ids must fit a hardened BIP32 index.
    #[error("app id {0} is too large for key derivation")]
    AppIdOutOfRange(u64),
    /// Wrong key, or the ciphertext was modified.
    #[error("decryption failed: wrong key or corrupted data")]
    Decrypt,
    /// The stored value is not in a format this version understands.
    #[error("unsupported encrypted value format")]
    Format,
    /// Key derivation from a user secret failed (invalid parameters).
    #[error("key derivation failed: {0}")]
    Kdf(String),
}
