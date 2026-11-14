//! Key ids, base64url helpers, Ed25519 signing and verification, pseudonyms.
//! See `docs/mvp-contract.md` ("Crypto and identity").

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub use ed25519_dalek::{SigningKey as Ed25519SigningKey, VerifyingKey as Ed25519VerifyingKey};

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("invalid base64url: {0}")]
    Base64(String),
    #[error("public key must be 32 bytes (43 base64url characters)")]
    PublicKeyLength,
    #[error("invalid Ed25519 public key")]
    PublicKeyInvalid,
    #[error("private key must be 32 bytes (43 base64url characters)")]
    PrivateKeyLength,
    #[error("signature must be 64 bytes (86 base64url characters)")]
    SignatureLength,
    #[error("signed document must be a JSON object")]
    NotAnObject,
    #[error("signed document has no string `signature` member")]
    MissingSignature,
    #[error("signature does not verify")]
    BadSignature,
    #[error(transparent)]
    Jcs(#[from] crate::jcs::JcsError),
    #[error("invalid HMAC key")]
    HmacKey,
}

pub fn b64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64url_decode(s: &str) -> Result<Vec<u8>, CryptoError> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| CryptoError::Base64(e.to_string()))
}

/// Parse a 43-character base64url public key.
pub fn parse_public_key(s: &str) -> Result<VerifyingKey, CryptoError> {
    let bytes: [u8; 32] = b64url_decode(s)?
        .try_into()
        .map_err(|_| CryptoError::PublicKeyLength)?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| CryptoError::PublicKeyInvalid)
}

pub fn encode_public_key(key: &VerifyingKey) -> String {
    b64url_encode(key.as_bytes())
}

/// Parse a 43-character base64url private key (32-byte seed).
pub fn parse_signing_key(s: &str) -> Result<SigningKey, CryptoError> {
    let bytes: [u8; 32] = b64url_decode(s)?
        .try_into()
        .map_err(|_| CryptoError::PrivateKeyLength)?;
    Ok(SigningKey::from_bytes(&bytes))
}

pub fn encode_signing_key(key: &SigningKey) -> String {
    b64url_encode(&key.to_bytes())
}

/// A fresh keypair from the OS random source.
pub fn generate_signing_key() -> SigningKey {
    SigningKey::generate(&mut rand_core::OsRng)
}

/// `key_id` = base64url-nopad(SHA-256(raw public key)[..16]). Always 22 characters.
pub fn key_id(key: &VerifyingKey) -> String {
    let digest = Sha256::digest(key.as_bytes());
    b64url_encode(&digest[..16])
}

/// `key_id` of a base64url public key.
pub fn key_id_from_b64(public_key: &str) -> Result<String, CryptoError> {
    Ok(key_id(&parse_public_key(public_key)?))
}

/// UTF-8 of the JCS form of `doc` with the top-level `signature` member removed.
pub fn signed_bytes(doc: &Value) -> Result<Vec<u8>, CryptoError> {
    let obj = doc.as_object().ok_or(CryptoError::NotAnObject)?;
    let mut copy = obj.clone();
    copy.remove("signature");
    Ok(crate::jcs::to_vec(&Value::Object(copy))?)
}

/// Verify the `signature` member of `doc` against `key`.
pub fn verify_object(doc: &Value, key: &VerifyingKey) -> Result<(), CryptoError> {
    let sig_str = doc
        .get("signature")
        .and_then(Value::as_str)
        .ok_or(CryptoError::MissingSignature)?;
    let sig_bytes: [u8; 64] = b64url_decode(sig_str)?
        .try_into()
        .map_err(|_| CryptoError::SignatureLength)?;
    let sig = Signature::from_bytes(&sig_bytes);
    let msg = signed_bytes(doc)?;
    key.verify(&msg, &sig)
        .map_err(|_| CryptoError::BadSignature)
}

/// Return a copy of `doc` with `signature` set to a signature by `key`
/// (any existing `signature` is replaced).
pub fn sign_object(doc: &Value, key: &SigningKey) -> Result<Value, CryptoError> {
    let msg = signed_bytes(doc)?;
    let sig = key.sign(&msg);
    let mut out = doc.as_object().ok_or(CryptoError::NotAnObject)?.clone();
    out.insert(
        "signature".to_owned(),
        Value::String(b64url_encode(&sig.to_bytes())),
    );
    Ok(Value::Object(out))
}

/// Pseudonym = base64url-nopad(HMAC-SHA256(salt, key_id || anchor)) truncated to 22 characters.
pub fn pseudonym(salt: &[u8], key_id: &str, anchor: &str) -> Result<String, CryptoError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(salt).map_err(|_| CryptoError::HmacKey)?;
    mac.update(key_id.as_bytes());
    mac.update(anchor.as_bytes());
    let mut s = b64url_encode(&mac.finalize().into_bytes());
    s.truncate(22);
    Ok(s)
}

/// Lower-case hex of SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use serde_json::json;

    fn test_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn doc() -> Value {
        json!({ "b": 2, "a": "x", "signature": "ignored" })
    }

    #[test]
    fn signed_bytes_strip_signature_and_canonicalise() {
        assert_eq!(signed_bytes(&doc()).unwrap(), br#"{"a":"x","b":2}"#);
        assert!(signed_bytes(&json!([1])).is_err());
    }

    #[test]
    fn fixed_vectors() {
        let sk = test_key();
        let pk = sk.verifying_key();
        assert_eq!(encode_public_key(&pk), VEC_PUBLIC_KEY);
        assert_eq!(key_id(&pk), VEC_KEY_ID);
        assert_eq!(key_id(&pk).len(), 22);
        let signed = sign_object(&doc(), &sk).unwrap();
        assert_eq!(signed["signature"], VEC_SIGNATURE);
        assert_eq!(VEC_SIGNATURE.len(), 86);
        assert_eq!(
            pseudonym(&[1u8; 32], VEC_KEY_ID, "pl_01JC8Y0A1B2C3D4E5F6G7H8J9K").unwrap(),
            VEC_PSEUDONYM
        );
    }

    #[test]
    fn verify_and_tamper() {
        let sk = test_key();
        let pk = parse_public_key(VEC_PUBLIC_KEY).unwrap();
        let signed = sign_object(&doc(), &sk).unwrap();
        verify_object(&signed, &pk).unwrap();

        // Member order and whitespace do not matter.
        let reordered: Value = serde_json::from_str(&format!(
            r#"{{ "signature": "{VEC_SIGNATURE}", "a": "x", "b": 2.0 }}"#
        ))
        .unwrap();
        verify_object(&reordered, &pk).unwrap();

        let mut t = signed.clone();
        t["a"] = json!("y");
        assert!(matches!(
            verify_object(&t, &pk),
            Err(CryptoError::BadSignature)
        ));
        let mut t = signed.clone();
        t["c"] = json!(null);
        assert!(matches!(
            verify_object(&t, &pk),
            Err(CryptoError::BadSignature)
        ));
        let mut t = signed.clone();
        t.as_object_mut().unwrap().remove("b");
        assert!(matches!(
            verify_object(&t, &pk),
            Err(CryptoError::BadSignature)
        ));
        let mut t = signed.clone();
        t["signature"] = json!("AAAA");
        assert!(matches!(
            verify_object(&t, &pk),
            Err(CryptoError::SignatureLength)
        ));
        let mut t = signed.clone();
        t.as_object_mut().unwrap().remove("signature");
        assert!(matches!(
            verify_object(&t, &pk),
            Err(CryptoError::MissingSignature)
        ));

        let other = SigningKey::from_bytes(&[8u8; 32]).verifying_key();
        assert!(verify_object(&signed, &other).is_err());
    }

    #[test]
    fn key_roundtrip() {
        let sk = generate_signing_key();
        let enc = encode_signing_key(&sk);
        assert_eq!(enc.len(), 43);
        assert_eq!(parse_signing_key(&enc).unwrap().to_bytes(), sk.to_bytes());
        assert!(parse_public_key("abc").is_err());
    }

    #[test]
    fn pseudonym_differs_by_anchor() {
        let a = pseudonym(b"salt-salt-salt-salt-salt-salt-salt", "k", "pl_A").unwrap();
        let b = pseudonym(b"salt-salt-salt-salt-salt-salt-salt", "k", "pl_B").unwrap();
        assert_eq!(a.len(), 22);
        assert_ne!(a, b);
    }

    const VEC_PUBLIC_KEY: &str = "6kpsY-KcUgq-9VB7Ey7F-ZVHdq6-vnuSQh7qaRRG0iw";
    const VEC_KEY_ID: &str = "_oEsEvOrTOasXbaaw1L5Bg";
    const VEC_SIGNATURE: &str =
        "B8RP8gkCMV-F-YIyl8F4xoZpdDUf0C9LcqsRguKuUgiRFrGLIJKTzhHg73NoCvmKpJm-BxyVcAKH8nEi5OYmAg";
    // key_id and pseudonym cross-checked with Python hashlib/hmac.
    const VEC_PSEUDONYM: &str = "fIySb2Ko4cgpcJMft-bQ44";
}
