use std::fmt;

use bip39::{Language, Mnemonic};
use zeroize::Zeroizing;

use crate::KeyError;

/// A BIP39 recovery phrase (12 or 24 English words, no BIP39 passphrase).
///
/// `Debug` never prints the words.
#[derive(Clone)]
pub struct RecoveryPhrase {
    mnemonic: Mnemonic,
}

impl RecoveryPhrase {
    /// Generates a new 24-word phrase from the OS random number generator.
    pub fn generate() -> Result<Self, KeyError> {
        let mnemonic = Mnemonic::generate_in(Language::English, 24)
            .map_err(|e| KeyError::InvalidPhrase(e.to_string()))?;
        Ok(RecoveryPhrase { mnemonic })
    }

    /// Parses a phrase typed by a user: case and extra whitespace are ignored.
    pub fn parse(words: &str) -> Result<Self, KeyError> {
        let normalized = Zeroizing::new(words.split_whitespace().collect::<Vec<_>>().join(" "));
        let normalized = Zeroizing::new(normalized.to_lowercase());
        let mnemonic = Mnemonic::parse_in_normalized(Language::English, &normalized)
            .map_err(|e| KeyError::InvalidPhrase(e.to_string()))?;
        match mnemonic.word_count() {
            12 | 24 => Ok(RecoveryPhrase { mnemonic }),
            n => Err(KeyError::InvalidPhrase(format!("expected 12 or 24 words, got {n}"))),
        }
    }

    /// Number of words.
    pub fn word_count(&self) -> usize {
        self.mnemonic.word_count()
    }

    /// The words, space-separated. Handle with care: this is the wallet secret.
    pub fn expose_words(&self) -> Zeroizing<String> {
        Zeroizing::new(self.mnemonic.to_string())
    }

    /// The 64-byte BIP39 seed (empty passphrase).
    pub fn seed(&self) -> Zeroizing<[u8; 64]> {
        Zeroizing::new(self.mnemonic.to_seed(""))
    }
}

impl fmt::Debug for RecoveryPhrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RecoveryPhrase({} words, redacted)", self.word_count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTOR: &str = "abandon abandon abandon abandon abandon abandon abandon abandon \
                          abandon abandon abandon about";

    #[test]
    fn generates_24_words() {
        let p = RecoveryPhrase::generate().unwrap();
        assert_eq!(p.word_count(), 24);
        assert_ne!(
            p.expose_words().as_str(),
            RecoveryPhrase::generate().unwrap().expose_words().as_str()
        );
    }

    #[test]
    fn parses_messy_input() {
        let p = RecoveryPhrase::parse("  ABANDON abandon\nabandon abandon abandon abandon abandon abandon abandon abandon abandon About ").unwrap();
        assert_eq!(p.word_count(), 12);
        assert_eq!(
            p.expose_words().as_str(),
            VECTOR.split_whitespace().collect::<Vec<_>>().join(" ")
        );
    }

    #[test]
    fn bip39_seed_vector() {
        // BIP39 reference vector (empty passphrase).
        let seed = RecoveryPhrase::parse(VECTOR).unwrap().seed();
        assert_eq!(
            hex::encode(&seed[..]),
            "5eb00bbddcf069084889a8ab9155568165f5c453ccb85e70811aaed6f6da5fc1\
             9a5ac40b389cd370d086206dec8aa6c43daea6690f20ad3d8d48b2d2ce9e38e4"
        );
    }

    #[test]
    fn rejects_bad_checksum_and_lengths() {
        assert!(RecoveryPhrase::parse("abandon abandon abandon").is_err());
        assert!(RecoveryPhrase::parse(&VECTOR.replace("about", "abandon")).is_err());
        let fifteen = "abandon ".repeat(14) + "address";
        assert!(RecoveryPhrase::parse(&fifteen).is_err());
    }

    #[test]
    fn debug_is_redacted() {
        let p = RecoveryPhrase::parse(VECTOR).unwrap();
        let shown = format!("{p:?}");
        assert!(!shown.contains("abandon"));
        assert!(shown.contains("12 words"));
    }
}
