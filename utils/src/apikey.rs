// API key primitives (no IO).
//
// Client signs every request:
//   X-API-KEY:   nxs_live_<32 hex>
//   X-TIMESTAMP: unix seconds
//   X-SIGNATURE: hex(HMAC_SHA256(secret, timestamp + METHOD + path_and_query + body))
//
// Server recomputes the HMAC with the decrypted secret and compares in constant time.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const KEY_PREFIX: &str = "nxs_live_";
/// Requests older / newer than this are rejected (replay protection).
pub const MAX_CLOCK_SKEW_SECS: i64 = 30;

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// Returns (public key id, secret). The secret is shown to the user exactly once.
pub fn generate_key_pair() -> (String, String) {
    (format!("{KEY_PREFIX}{}", random_hex(16)), random_hex(32))
}

#[derive(Debug, PartialEq)]
pub enum CryptoError {
    BadEncryptionKey,
    Encrypt,
    Decrypt,
}

/// `API_KEY_ENCRYPTION_KEY` is 32 bytes as 64 hex chars.
pub fn parse_encryption_key(hex_key: &str) -> Result<[u8; 32], CryptoError> {
    let bytes = hex::decode(hex_key.trim()).map_err(|_| CryptoError::BadEncryptionKey)?;
    bytes.try_into().map_err(|_| CryptoError::BadEncryptionKey)
}

/// Returns (ciphertext, nonce).
pub fn encrypt_secret(key: &[u8; 32], secret: &str) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), secret.as_bytes())
        .map_err(|_| CryptoError::Encrypt)?;
    Ok((ciphertext, nonce.to_vec()))
}

pub fn decrypt_secret(key: &[u8; 32], ciphertext: &[u8], nonce: &[u8]) -> Result<String, CryptoError> {
    if nonce.len() != 12 {
        return Err(CryptoError::Decrypt);
    }
    let cipher = Aes256Gcm::new(key.into());
    let plain = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| CryptoError::Decrypt)?;
    String::from_utf8(plain).map_err(|_| CryptoError::Decrypt)
}

fn mac(secret: &str, timestamp: &str, method: &str, path: &str, body: &[u8]) -> HmacSha256 {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(timestamp.as_bytes());
    mac.update(method.to_uppercase().as_bytes());
    mac.update(path.as_bytes());
    mac.update(body);
    mac
}

/// What a client computes (also used in tests / SDKs).
pub fn sign(secret: &str, timestamp: &str, method: &str, path: &str, body: &[u8]) -> String {
    hex::encode(mac(secret, timestamp, method, path, body).finalize().into_bytes())
}

/// Constant-time signature check.
pub fn verify_signature(
    secret: &str,
    timestamp: &str,
    method: &str,
    path: &str,
    body: &[u8],
    signature_hex: &str,
) -> bool {
    let Ok(sig) = hex::decode(signature_hex) else {
        return false;
    };
    mac(secret, timestamp, method, path, body)
        .verify_slice(&sig)
        .is_ok()
}

/// Timestamp must be unix seconds within MAX_CLOCK_SKEW_SECS of `now`.
pub fn timestamp_fresh(timestamp: &str, now: i64) -> bool {
    timestamp
        .parse::<i64>()
        .is_ok_and(|ts| (now - ts).abs() <= MAX_CLOCK_SKEW_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_format() {
        let (key, secret) = generate_key_pair();
        assert!(key.starts_with(KEY_PREFIX));
        assert_eq!(key.len(), KEY_PREFIX.len() + 32);
        assert_eq!(secret.len(), 64);
    }

    #[test]
    fn encrypt_roundtrip_and_tamper() {
        let k = [7u8; 32];
        let (ct, nonce) = encrypt_secret(&k, "s3cret").unwrap();
        assert_eq!(decrypt_secret(&k, &ct, &nonce).unwrap(), "s3cret");
        assert_eq!(decrypt_secret(&[8u8; 32], &ct, &nonce), Err(CryptoError::Decrypt));
        let mut bad = ct.clone();
        bad[0] ^= 1;
        assert_eq!(decrypt_secret(&k, &bad, &nonce), Err(CryptoError::Decrypt));
    }

    #[test]
    fn signature_roundtrip() {
        let body = br#"{"pair":"BTC-USDT"}"#;
        let sig = sign("secret", "1710000000", "post", "/orders", body);
        assert!(verify_signature("secret", "1710000000", "POST", "/orders", body, &sig));
        assert!(!verify_signature("secret", "1710000001", "POST", "/orders", body, &sig));
        assert!(!verify_signature("other", "1710000000", "POST", "/orders", body, &sig));
        assert!(!verify_signature("secret", "1710000000", "POST", "/orders", b"{}", &sig));
        assert!(!verify_signature("secret", "1710000000", "POST", "/orders", body, "zz"));
    }

    #[test]
    fn freshness() {
        assert!(timestamp_fresh("1000", 1010));
        assert!(!timestamp_fresh("1000", 1031));
        assert!(!timestamp_fresh("abc", 1000));
    }

    #[test]
    fn encryption_key_parsing() {
        assert!(parse_encryption_key(&"ab".repeat(32)).is_ok());
        assert!(parse_encryption_key("abcd").is_err());
    }
}
