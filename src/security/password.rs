use std::fmt;

use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{
        PasswordHash as ParsedPasswordHash, PasswordHasher, PasswordVerifier, SaltString,
    },
};
use secrecy::{ExposeSecret, SecretString};
use uuid::Uuid;

const MIN_PASSWORD_LENGTH: usize = 12;
pub const MAX_PASSWORD_LENGTH: usize = 512;

/// A PHC-encoded Argon2id hash. Hashes may be persisted; plaintext may not.
/// Persistence goes through `encoded()`/`parse()` — never serde.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredPasswordHash(String);

impl fmt::Debug for StoredPasswordHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoredPasswordHash([REDACTED])")
    }
}

impl StoredPasswordHash {
    /// # Errors
    ///
    /// Returns [`PasswordError::InvalidStoredHash`] unless this is a valid Argon2id PHC string.
    pub fn parse(encoded: String) -> Result<Self, PasswordError> {
        let parsed =
            ParsedPasswordHash::new(&encoded).map_err(|_| PasswordError::InvalidStoredHash)?;
        if parsed.algorithm.as_str() != "argon2id" {
            return Err(PasswordError::InvalidStoredHash);
        }
        Ok(Self(encoded))
    }

    #[must_use]
    pub fn encoded(&self) -> &str {
        &self.0
    }
}

/// A transient secret accepted only from a private Discord modal or after its source message was deleted.
pub struct PasswordSubmission(SecretString);

impl fmt::Debug for PasswordSubmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PasswordSubmission([REDACTED])")
    }
}

impl PasswordSubmission {
    #[must_use]
    pub fn from_private_modal(password: SecretString) -> Self {
        Self(password)
    }

    /// # Errors
    ///
    /// Returns [`PasswordError::SourceMessageNotDeleted`] until Discord confirms deletion.
    pub fn try_after_delete(
        password: SecretString,
        source_message_deleted: bool,
    ) -> Result<Self, PasswordError> {
        if !source_message_deleted {
            return Err(PasswordError::SourceMessageNotDeleted);
        }
        Ok(Self(password))
    }

    fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PasswordError {
    #[error("delete the source message before password verification")]
    SourceMessageNotDeleted,
    #[error("password must contain between 12 and 512 characters")]
    InvalidPasswordLength,
    #[error("stored password hash is not a valid Argon2id PHC string")]
    InvalidStoredHash,
    #[error("password hashing failed")]
    HashingFailed,
}

#[derive(Clone)]
pub struct Argon2idPasswordManager {
    argon2: Argon2<'static>,
}

impl Default for Argon2idPasswordManager {
    fn default() -> Self {
        let params =
            Params::new(65_536, 3, 4, Some(32)).expect("fixed Argon2id parameters must be valid");
        Self {
            argon2: Argon2::new(Algorithm::Argon2id, Version::V0x13, params),
        }
    }
}

impl Argon2idPasswordManager {
    /// # Errors
    ///
    /// Returns an error when the password length is unsafe or Argon2id hashing fails.
    #[allow(clippy::needless_pass_by_value)] // Ownership bounds the plaintext lifetime.
    pub fn hash(&self, password: SecretString) -> Result<StoredPasswordHash, PasswordError> {
        validate_password_length(password.expose_secret())?;
        // UUID v4 supplies 122 random bits; SaltString encodes the full 128-bit byte array.
        let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
            .map_err(|_| PasswordError::HashingFailed)?;
        let encoded = self
            .argon2
            .hash_password(password.expose_secret().as_bytes(), &salt)
            .map_err(|_| PasswordError::HashingFailed)?
            .to_string();
        StoredPasswordHash::parse(encoded)
    }

    #[must_use]
    pub fn verify(
        &self,
        stored_hash: &StoredPasswordHash,
        submission: &PasswordSubmission,
    ) -> bool {
        if validate_password_length(submission.expose()).is_err() {
            return false;
        }
        let Ok(parsed) = ParsedPasswordHash::new(stored_hash.encoded()) else {
            return false;
        };
        self.argon2
            .verify_password(submission.expose().as_bytes(), &parsed)
            .is_ok()
    }
}

fn validate_password_length(password: &str) -> Result<(), PasswordError> {
    if !(MIN_PASSWORD_LENGTH..=MAX_PASSWORD_LENGTH).contains(&password.chars().count()) {
        return Err(PasswordError::InvalidPasswordLength);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_with_argon2id_and_redacts_debug() {
        let manager = Argon2idPasswordManager::default();
        let hash = manager
            .hash(SecretString::from(
                "correct horse battery staple".to_owned(),
            ))
            .unwrap();
        assert!(hash.encoded().starts_with("$argon2id$"));
        assert_eq!(format!("{hash:?}"), "StoredPasswordHash([REDACTED])");

        let submission = PasswordSubmission::try_after_delete(
            SecretString::from("correct horse battery staple".to_owned()),
            true,
        )
        .unwrap();
        assert!(manager.verify(&hash, &submission));
        assert!(!format!("{submission:?}").contains("correct"));
    }

    #[test]
    fn refuses_secret_until_source_message_is_deleted() {
        let result = PasswordSubmission::try_after_delete(
            SecretString::from("correct horse battery staple".to_owned()),
            false,
        );
        assert_eq!(result.unwrap_err(), PasswordError::SourceMessageNotDeleted);
    }

    #[test]
    fn password_limit_matches_discord_modal_capacity() {
        assert!(validate_password_length(&"x".repeat(MAX_PASSWORD_LENGTH)).is_ok());
        assert_eq!(
            validate_password_length(&"x".repeat(MAX_PASSWORD_LENGTH + 1)),
            Err(PasswordError::InvalidPasswordLength)
        );
    }
}
