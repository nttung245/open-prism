//! Password hashing (argon2id) and the password policy (`user-accounts` spec).
//!
//! Hashing takes tens of milliseconds and ~19 MiB on purpose: callers run it in
//! `spawn_blocking`, never on an async worker.

use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};

/// Shortest password accepted.
pub const MIN_LENGTH: usize = 12;
/// Longest password accepted: argon2 hashes anything, but a megabyte "password" is an attack.
pub const MAX_LENGTH: usize = 1024;

/// Why a password is refused, as shown to the person choosing it.
pub fn check_policy(password: &str) -> Result<(), String> {
    let length = password.chars().count();
    if length < MIN_LENGTH {
        return Err(format!(
            "choose a password of at least {MIN_LENGTH} characters"
        ));
    }
    if password.len() > MAX_LENGTH {
        return Err(format!("a password can have at most {MAX_LENGTH} bytes"));
    }
    Ok(())
}

/// A PHC string (`$argon2id$v=19$…`) with a fresh random salt.
pub fn hash(password: &str) -> anyhow::Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|err| anyhow::anyhow!("cannot hash the password: {err}"))
}

/// Whether `password` matches `stored`. A malformed stored hash never matches.
pub fn verify(stored: &str, password: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// Spends the time of one verification without a real hash, so that a sign-in with an
/// unknown user name takes as long as one with a wrong password.
pub fn verify_nothing(password: &str) {
    static DUMMY: OnceLock<String> = OnceLock::new();
    let dummy = DUMMY.get_or_init(|| hash("not a password of anyone").unwrap_or_default());
    let _ = verify(dummy, password);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_verify_and_salts_differ() {
        let first = hash("correct horse battery").unwrap();
        let second = hash("correct horse battery").unwrap();
        assert!(first.starts_with("$argon2id$"));
        assert_ne!(first, second);
        assert!(verify(&first, "correct horse battery"));
        assert!(!verify(&first, "correct horse batterY"));
        assert!(!verify("not a hash", "anything"));
    }

    #[test]
    fn the_policy_wants_twelve_characters() {
        assert!(check_policy("short pass").unwrap_err().contains("12"));
        assert!(check_policy("twelve chars").is_ok());
        // Counted in characters, not bytes.
        assert!(check_policy("mật khẩu dài").is_ok());
        assert!(check_policy(&"x".repeat(MAX_LENGTH + 1)).is_err());
    }
}
