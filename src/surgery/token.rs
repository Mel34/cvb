use std::fs::File;
use std::io::Read;

pub(crate) const TOKEN_LEN: usize = 16;

const BASE62_ALPHABET: &[u8; 62] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

// 62 * 4 = 248. Reject bytes >= 248 to eliminate modulo bias.
const REJECTION_THRESHOLD: u8 = 248;

/// Generates a 16-character alphanumeric token from `/dev/urandom`.
pub(crate) fn generate_token() -> Result<String, String> {
    let mut file = File::open("/dev/urandom")
        .map_err(|e| format!("CVB: failed to open /dev/urandom: {e}"))?;

    let mut token = String::with_capacity(TOKEN_LEN);
    let mut batch = [0u8; 32];

    while token.len() < TOKEN_LEN {
        file.read_exact(&mut batch)
            .map_err(|e| format!("CVB: failed to read /dev/urandom: {e}"))?;

        for &byte in &batch {
            if byte < REJECTION_THRESHOLD {
                token.push(BASE62_ALPHABET[(byte % 62) as usize] as char);
                if token.len() == TOKEN_LEN {
                    break;
                }
            }
        }
    }

    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surgery::parse::valid_token;
    use std::collections::HashSet;

    #[test]
    fn generated_token_has_correct_length_and_is_valid() {
        let token = generate_token().expect("token generation should succeed");

        assert_eq!(token.len(), TOKEN_LEN);
        assert!(valid_token(&token));
        assert!(token.bytes().all(|byte| byte.is_ascii_alphanumeric()));
    }

    #[test]
    fn generated_tokens_are_distinct_in_a_batch() {
        let mut seen = HashSet::new();

        for _ in 0..100 {
            let token = generate_token().expect("token generation should succeed");

            assert_eq!(token.len(), TOKEN_LEN);
            assert!(valid_token(&token));
            assert!(seen.insert(token), "duplicate token generated in test batch");
        }
    }
}
