//! M9b: encryption of synced notes. Pure functions, no I/O.
//!
//! Blob format: `b"CTG1"` + nonce (24 random bytes) + XChaCha20-Poly1305 ciphertext with its tag.
//! The AAD is the relative path, so a blob cannot be swapped onto another path.
//! A file's `rev` is the lowercase hex SHA-256 of its plaintext.

use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};

pub const MAGIC: &[u8; 4] = b"CTG1";
pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;

pub type Key = [u8; KEY_LEN];

/// Lowercase hex SHA-256 of the plaintext.
pub fn rev_of(bytes: &[u8]) -> String {
    to_hex(&Sha256::digest(bytes))
}

pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// Standard base64 (with or without padding) as the server sends the key.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    let body = text.trim_end_matches('=');
    if text.len() - body.len() > 2 {
        return None;
    }
    for c in body.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    // Leftover bits must be zero padding.
    (acc & ((1 << bits) - 1) == 0).then_some(out)
}

/// The notes key from the server's base64, which must be exactly 32 bytes.
pub fn key_from_base64(text: &str) -> Result<Key, String> {
    base64_decode(text.trim())
        .and_then(|b| Key::try_from(b).ok())
        .ok_or_else(|| "the server sent an invalid notes key".to_string())
}

pub fn encrypt(key: &Key, path: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let nonce = XNonce::try_generate().map_err(|e| format!("no randomness for encryption: {e}"))?;
    let cipher = XChaCha20Poly1305::new(&(*key).into());
    let sealed = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: path.as_bytes(),
            },
        )
        .map_err(|_| "encrypting failed".to_string())?;
    let mut blob = Vec::with_capacity(MAGIC.len() + NONCE_LEN + sealed.len());
    blob.extend_from_slice(MAGIC);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&sealed);
    Ok(blob)
}

/// Fails for a wrong key, a different path, a damaged blob or a foreign format.
pub fn decrypt(key: &Key, path: &str, blob: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || format!("{path} could not be decrypted (wrong key, other path or damaged data)");
    if blob.len() < MAGIC.len() + NONCE_LEN || &blob[..MAGIC.len()] != MAGIC {
        return Err(format!("{path} is not a Catenga encrypted file"));
    }
    let nonce = XNonce::try_from(&blob[MAGIC.len()..MAGIC.len() + NONCE_LEN]).map_err(|_| bad())?;
    XChaCha20Poly1305::new(&(*key).into())
        .decrypt(
            &nonce,
            Payload {
                msg: &blob[MAGIC.len() + NONCE_LEN..],
                aad: path.as_bytes(),
            },
        )
        .map_err(|_| bad())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: Key = [7; KEY_LEN];
    const PATH: &str = "2026/10/2026-10-20.md";

    #[test]
    fn round_trip_and_blob_layout() {
        let blob = encrypt(&KEY, PATH, "olá, notas\n".as_bytes()).unwrap();
        assert_eq!(&blob[..4], b"CTG1");
        assert_eq!(blob.len(), 4 + 24 + "olá, notas\n".len() + 16);
        assert_eq!(
            decrypt(&KEY, PATH, &blob).unwrap(),
            "olá, notas\n".as_bytes()
        );
        // Empty files work too.
        let empty = encrypt(&KEY, PATH, b"").unwrap();
        assert_eq!(decrypt(&KEY, PATH, &empty).unwrap(), b"");
    }

    #[test]
    fn every_upload_uses_a_fresh_nonce() {
        let a = encrypt(&KEY, PATH, b"same").unwrap();
        let b = encrypt(&KEY, PATH, b"same").unwrap();
        assert_ne!(a[4..28], b[4..28]);
        assert_ne!(a, b);
    }

    #[test]
    fn the_path_is_bound_to_the_blob() {
        let blob = encrypt(&KEY, PATH, b"secret").unwrap();
        let err = decrypt(&KEY, "2026/10/2026-10-21.md", &blob).unwrap_err();
        assert!(err.contains("could not be decrypted"), "{err}");
    }

    #[test]
    fn tampering_a_wrong_key_and_foreign_data_are_refused() {
        let blob = encrypt(&KEY, PATH, b"secret text").unwrap();
        for i in [0, 5, 30, blob.len() - 1] {
            let mut bad = blob.clone();
            bad[i] ^= 1;
            assert!(decrypt(&KEY, PATH, &bad).is_err(), "byte {i}");
        }
        assert!(decrypt(&[8; KEY_LEN], PATH, &blob).is_err());
        assert!(decrypt(&KEY, PATH, &blob[..20]).is_err());
        assert!(decrypt(&KEY, PATH, b"").is_err());
        assert!(decrypt(&KEY, PATH, b"plain text, not a blob at all......").is_err());
    }

    #[test]
    fn rev_is_the_lowercase_hex_sha256_of_the_plaintext() {
        assert_eq!(
            rev_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            rev_of(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hex_round_trips_and_rejects_garbage() {
        assert_eq!(to_hex(&[0, 15, 255]), "000fff");
        assert_eq!(from_hex("000fff"), Some(vec![0, 15, 255]));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
        assert_eq!(from_hex("é"), None);
    }

    #[test]
    fn base64_decodes_with_and_without_padding() {
        assert_eq!(base64_decode("TWFu"), Some(b"Man".to_vec()));
        assert_eq!(base64_decode("TWE="), Some(b"Ma".to_vec()));
        assert_eq!(base64_decode("TWE"), Some(b"Ma".to_vec()));
        assert_eq!(base64_decode("TQ=="), Some(b"M".to_vec()));
        assert_eq!(base64_decode("+/8="), Some(vec![0xfb, 0xff]));
        assert_eq!(base64_decode(""), Some(vec![]));
        assert_eq!(base64_decode("TW!u"), None);
        assert_eq!(base64_decode("TWF==="), None);
    }

    #[test]
    fn the_server_key_must_be_exactly_32_bytes() {
        let ok = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
        assert_eq!(key_from_base64(ok).unwrap()[..3], [0, 1, 2]);
        assert_eq!(key_from_base64(&format!(" {ok}\n")).unwrap()[31], 31);
        assert!(key_from_base64("AAEC").is_err());
        assert!(key_from_base64("not base64!").is_err());
    }
}
