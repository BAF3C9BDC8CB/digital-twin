use sha1::{Digest, Sha1};
use sha2::Sha256;

pub fn sha1_hex(data: &str) -> String {
    let mut h = Sha1::new();
    h.update(data.as_bytes());
    hex::encode(h.finalize())
}

pub fn sha256_hex(data: &str) -> String {
    let mut h = Sha256::new();
    h.update(data.as_bytes());
    hex::encode(h.finalize())
}

pub fn sha256_truncated_hex(data: &str) -> String {
    hex::encode(&Sha256::digest(data.as_bytes())[..20])
}

pub fn method_id_to_u64(method_id: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(method_id.as_bytes());
    let result = hasher.finalize();
    u64::from_be_bytes(result[..8].try_into().unwrap())
}
