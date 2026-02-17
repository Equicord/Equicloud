use sha2::{Digest, Sha256};

/// CRC32-based hash functions (kept for migration compatibility)
pub mod legacy {
    #[allow(deprecated)]
    pub fn hash_user_id(user_id: &str) -> String {
        let user_hash = crc32fast::hash(user_id.as_bytes());
        format!("settings:{}", user_hash)
    }

    #[allow(deprecated)]
    pub fn get_user_secret(user_id: &str) -> String {
        let user_hash = crc32fast::hash(user_id.as_bytes());
        format!("{:08x}", user_hash)
    }
}

pub mod sha256 {
    use super::*;

    pub fn hash_user_id(user_id: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(user_id.as_bytes());
        let result = hasher.finalize();
        format!("settings:{}", hex::encode(&result[..8]))
    }

    pub fn get_user_secret(user_id: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"secret:");
        hasher.update(user_id.as_bytes());
        let result = hasher.finalize();
        hex::encode(&result[..16])
    }
}

pub fn is_legacy_key(key: &str) -> bool {
    if let Some(hash_part) = key.strip_prefix("settings:") {
        !hash_part.is_empty()
            && hash_part.len() <= 10
            && hash_part.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_legacy_key() {
        assert!(is_legacy_key("settings:1234567890"));
        assert!(is_legacy_key("settings:123"));

        assert!(!is_legacy_key("settings:a1b2c3d4e5f6g7h8"));
        assert!(!is_legacy_key("settings:1a2b3c4d5e6f7890"));

        assert!(!is_legacy_key("invalid:123"));
        assert!(!is_legacy_key("settings:"));
    }

    #[test]
    fn test_hash_formats_differ() {
        let user_id = "123456789";

        let legacy_key = legacy::hash_user_id(user_id);
        let new_key = sha256::hash_user_id(user_id);

        assert_ne!(legacy_key, new_key, "Hash formats should differ");
        assert!(is_legacy_key(&legacy_key), "Legacy key should be detected");
        assert!(!is_legacy_key(&new_key), "New key should not be legacy");
    }
}
