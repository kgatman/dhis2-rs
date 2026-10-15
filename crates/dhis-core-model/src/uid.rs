//! `org.hisp.dhis.common.CodeGenerator` (UID generation) and UID validation.

use rand::RngExt;

const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const ALPHANUMERIC: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// `CodeGenerator.UID_CODE_SIZE`.
pub const UID_CODE_SIZE: usize = 11;

/// `CodeGenerator.generateCode(int)`: first char a letter, the rest alphanumeric
/// (non-cryptographic random, like Java's `ThreadLocalRandom`).
pub fn generate_code(size: usize) -> String {
    let mut r = rand::rng();
    let mut out = String::with_capacity(size);
    if size > 0 {
        out.push(LETTERS[r.random_range(0..LETTERS.len())] as char);
    }
    for _ in 1..size {
        out.push(ALPHANUMERIC[r.random_range(0..ALPHANUMERIC.len())] as char);
    }
    out
}

/// `CodeGenerator.generateUid()`.
pub fn generate_uid() -> String {
    generate_code(UID_CODE_SIZE)
}

/// `CodeGenerator.isValidUid(String)`: `^[a-zA-Z]{1}[a-zA-Z0-9]{10}$`.
pub fn is_valid_uid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == UID_CODE_SIZE
        && b[0].is_ascii_alphabetic()
        && b.iter().all(u8::is_ascii_alphanumeric)
}

/// `UUID.randomUUID().toString()`.
pub fn random_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_uids_are_valid() {
        for _ in 0..1000 {
            let u = generate_uid();
            assert!(is_valid_uid(&u), "{u}");
        }
        assert!(!is_valid_uid("1abcdefghij"));
        assert!(!is_valid_uid("abc"));
    }

    #[test]
    fn uuid_format() {
        let u = random_uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(u, u.to_lowercase());
    }
}
