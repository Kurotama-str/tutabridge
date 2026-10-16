//! Bridge password verification. `bridge_password` in config.toml is either
//! the plaintext password or, when it starts with `$argon2id$`, an Argon2id
//! PHC hash of it. The mode is detected from the value itself.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

const ARGON2_PREFIX: &str = "$argon2id$";

pub fn is_hashed(configured: &str) -> bool {
    configured.starts_with(ARGON2_PREFIX)
}

/// Whether `supplied` matches the configured bridge password.
pub fn verify(supplied: &str, configured: &str) -> bool {
    if !is_hashed(configured) {
        return supplied == configured;
    }
    match PasswordHash::new(configured) {
        Ok(parsed) => Argon2::default()
            .verify_password(supplied.as_bytes(), &parsed)
            .is_ok(),
        Err(e) => {
            log::error!("bridge_password is not a valid Argon2 hash: {e}");
            false
        }
    }
}

pub fn hash(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_mode() {
        assert!(verify("abc", "abc"));
        assert!(!verify("abd", "abc"));
    }

    #[test]
    fn argon2_mode_is_detected_and_verifies() {
        let h = hash("s3cret-pw").unwrap();
        assert!(is_hashed(&h));
        assert!(verify("s3cret-pw", &h));
        assert!(!verify("s3cret-px", &h));
        assert!(!verify(&h, &h));
    }

    #[test]
    fn malformed_hash_rejects() {
        assert!(!verify("x", "$argon2id$garbage"));
    }
}
