//! PuTTY `.ppk` private keys, read natively so PuTTY users need no conversion
//! step. Versions 2 and 3, encrypted (aes256-cbc) or not; RSA, ECDSA (nistp256/
//! 384/521) and Ed25519. DSA is parsed but rejected because russh cannot use it.
//!
//! The file is decrypted and MAC-checked here, then re-encoded as an unencrypted
//! OpenSSH `openssh-key-v1` blob in memory and handed to russh-keys, which does
//! the key-type specific validation (and rejects a key whose halves disagree).
//!
//! Format reference: PuTTY manual, appendix "PPK file format".

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub fn is_ppk(text: &str) -> bool {
    text.trim_start_matches('\u{feff}').starts_with("PuTTY-User-Key-File-")
}

/// Parsed, decrypted and MAC-verified key material.
pub struct PpkKey {
    pub algorithm: String,
    pub comment: String,
    pub public_blob: Vec<u8>,
    /// Plaintext private blob (may carry trailing cipher padding).
    pub private_blob: Vec<u8>,
}

struct Header {
    version: u8,
    algorithm: String,
    fields: HashMap<String, String>,
    public_blob: Vec<u8>,
    private_blob: Vec<u8>,
}

fn parse_header(text: &str) -> Result<Header> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines().map(str::trim_end);
    let first = lines.next().ok_or_else(|| anyhow!("empty file"))?;
    let (tag, algorithm) = first.split_once(':').ok_or_else(|| anyhow!("not a PuTTY key file"))?;
    let version: u8 = tag
        .strip_prefix("PuTTY-User-Key-File-")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow!("not a PuTTY key file"))?;
    if version != 2 && version != 3 {
        bail!("PPK version {version} is not supported (only 2 and 3)");
    }
    let algorithm = algorithm.trim().to_string();

    let mut fields = HashMap::new();
    let mut public_blob = Vec::new();
    let mut private_blob = Vec::new();
    while let Some(line) = lines.next() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "Public-Lines" | "Private-Lines" => {
                let n: usize = v.parse().with_context(|| format!("bad {k}"))?;
                let mut b64 = String::new();
                for _ in 0..n {
                    b64.push_str(lines.next().ok_or_else(|| anyhow!("truncated {k}"))?.trim());
                }
                let blob = B64.decode(&b64).with_context(|| format!("decode {k}"))?;
                if k == "Public-Lines" { public_blob = blob } else { private_blob = blob }
            }
            _ => {
                fields.insert(k.to_string(), v.to_string());
            }
        }
    }
    if public_blob.is_empty() || private_blob.is_empty() {
        bail!("PPK is missing its key blobs");
    }
    Ok(Header { version, algorithm, fields, public_blob, private_blob })
}

/// Decrypt + verify. `passphrase` is ignored for unencrypted files.
pub fn parse(text: &str, passphrase: Option<&str>) -> Result<PpkKey> {
    let h = parse_header(text)?;
    let encryption = h.fields.get("Encryption").map(String::as_str).unwrap_or("none");
    let comment = h.fields.get("Comment").cloned().unwrap_or_default();
    let mac_hex = h.fields.get("Private-MAC").ok_or_else(|| anyhow!("PPK has no Private-MAC"))?;
    let mac_expected = data_encoding::HEXLOWER_PERMISSIVE.decode(mac_hex.as_bytes()).context("bad Private-MAC")?;

    let encrypted = match encryption {
        "none" => false,
        "aes256-cbc" => true,
        other => bail!("PPK encryption {other:?} is not supported"),
    };
    if encrypted && passphrase.map_or(true, str::is_empty) {
        bail!("this .ppk is passphrase-protected; add the passphrase to the credential");
    }
    // An unencrypted file's MAC is keyed with the empty passphrase, whatever was supplied.
    let pass = if encrypted { passphrase.unwrap_or("") } else { "" }.as_bytes();

    // Cipher key / IV / MAC key derivation differs per version.
    let (cipher_key, iv, mac_key): (Vec<u8>, [u8; 16], Vec<u8>) = if h.version == 2 {
        let key = if encrypted {
            let mut k = Vec::with_capacity(40);
            for i in 0u32..2 {
                let mut d = Sha1::new();
                d.update(i.to_be_bytes());
                d.update(pass);
                k.extend_from_slice(&d.finalize());
            }
            k.truncate(32);
            k
        } else {
            Vec::new()
        };
        let mut d = Sha1::new();
        d.update(b"putty-private-key-file-mac-key");
        d.update(pass);
        (key, [0u8; 16], d.finalize().to_vec())
    } else if encrypted {
        let out = argon2_v3(&h.fields, pass)?;
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&out[32..48]);
        (out[..32].to_vec(), iv, out[48..80].to_vec())
    } else {
        (Vec::new(), [0u8; 16], Vec::new())
    };

    let private_blob = if encrypted {
        use aes::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
        if h.private_blob.len() % 16 != 0 {
            bail!("encrypted private blob is not a multiple of the AES block size");
        }
        let mut buf = h.private_blob.clone();
        cbc::Decryptor::<aes::Aes256>::new_from_slices(&cipher_key, &iv)
            .map_err(|e| anyhow!("aes key/iv: {e}"))?
            .decrypt_padded_mut::<NoPadding>(&mut buf)
            .map_err(|e| anyhow!("aes decrypt: {e}"))?;
        buf
    } else {
        h.private_blob.clone()
    };

    // MAC over: string(alg) string(encryption) string(comment) string(pub) string(priv).
    let mut data = Vec::new();
    for part in [h.algorithm.as_bytes(), encryption.as_bytes(), comment.as_bytes(), &h.public_blob, &private_blob] {
        put_string(&mut data, part);
    }
    let ok = if h.version == 2 {
        let mut m = Hmac::<Sha1>::new_from_slice(&mac_key).expect("hmac accepts any key length");
        m.update(&data);
        m.verify_slice(&mac_expected).is_ok()
    } else {
        let mut m = Hmac::<Sha256>::new_from_slice(&mac_key).expect("hmac accepts any key length");
        m.update(&data);
        m.verify_slice(&mac_expected).is_ok()
    };
    if !ok {
        bail!(if encrypted { "wrong passphrase for .ppk (MAC mismatch)" } else { ".ppk is corrupt (MAC mismatch)" });
    }

    Ok(PpkKey { algorithm: h.algorithm, comment, public_blob: h.public_blob, private_blob })
}

fn argon2_v3(fields: &HashMap<String, String>, pass: &[u8]) -> Result<[u8; 80]> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let get = |k: &str| fields.get(k).ok_or_else(|| anyhow!("PPK v3 is missing {k}"));
    let algorithm = match get("Key-Derivation")?.as_str() {
        "Argon2id" => Algorithm::Argon2id,
        "Argon2i" => Algorithm::Argon2i,
        "Argon2d" => Algorithm::Argon2d,
        other => bail!("unsupported key derivation {other:?}"),
    };
    let m: u32 = get("Argon2-Memory")?.parse().context("Argon2-Memory")?;
    let t: u32 = get("Argon2-Passes")?.parse().context("Argon2-Passes")?;
    let p: u32 = get("Argon2-Parallelism")?.parse().context("Argon2-Parallelism")?;
    let salt = data_encoding::HEXLOWER_PERMISSIVE.decode(get("Argon2-Salt")?.as_bytes()).context("Argon2-Salt")?;
    let params = Params::new(m, t, p, Some(80)).map_err(|e| anyhow!("argon2 params: {e}"))?;
    let mut out = [0u8; 80];
    Argon2::new(algorithm, Version::V0x13, params)
        .hash_password_into(pass, &salt, &mut out)
        .map_err(|e| anyhow!("argon2: {e}"))?;
    Ok(out)
}

// ---------------------------------------------------------------- SSH wire helpers

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    put_u32(out, s.len() as u32);
    out.extend_from_slice(s);
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn string(&mut self) -> Result<&'a [u8]> {
        let len_bytes = self.buf.get(self.pos..self.pos + 4).ok_or_else(|| anyhow!("truncated key blob"))?;
        let len = u32::from_be_bytes(len_bytes.try_into().unwrap()) as usize;
        let s = self.buf.get(self.pos + 4..self.pos + 4 + len).ok_or_else(|| anyhow!("truncated key blob"))?;
        self.pos += 4 + len;
        Ok(s)
    }
}

/// Re-encode as an unencrypted `openssh-key-v1` PEM that russh-keys understands.
pub fn to_openssh_pem(key: &PpkKey) -> Result<String> {
    let mut pubr = Reader::new(&key.public_blob);
    let pub_alg = pubr.string()?;
    if pub_alg != key.algorithm.as_bytes() {
        bail!("public blob algorithm does not match header");
    }
    let mut privr = Reader::new(&key.private_blob);

    // Private section: key-type specific fields in OpenSSH order.
    let mut sec = Vec::new();
    match key.algorithm.as_str() {
        "ssh-ed25519" => {
            let public = pubr.string()?;
            let seed = privr.string()?;
            if public.len() != 32 || seed.len() != 32 {
                bail!("malformed ed25519 key");
            }
            put_string(&mut sec, public);
            let mut sk = seed.to_vec();
            sk.extend_from_slice(public);
            put_string(&mut sec, &sk);
        }
        "ssh-rsa" => {
            let e = pubr.string()?;
            let n = pubr.string()?;
            let d = privr.string()?;
            let p = privr.string()?;
            let q = privr.string()?;
            let iqmp = privr.string()?;
            for m in [n, e, d, iqmp, p, q] {
                put_string(&mut sec, m);
            }
        }
        a if a.starts_with("ecdsa-sha2-nistp") => {
            let curve = pubr.string()?;
            let point = pubr.string()?;
            let d = privr.string()?;
            put_string(&mut sec, curve);
            put_string(&mut sec, point);
            put_string(&mut sec, d);
        }
        "ssh-dss" => bail!("DSA keys are not supported by Charon's SSH client; generate an Ed25519 or RSA key"),
        other => bail!("unsupported key type {other:?}"),
    }

    let mut private = Vec::new();
    let check: u32 = 0x6368_726e; // any value, repeated twice
    put_u32(&mut private, check);
    put_u32(&mut private, check);
    put_string(&mut private, key.algorithm.as_bytes());
    private.extend_from_slice(&sec);
    put_string(&mut private, key.comment.as_bytes());
    let mut pad = 1u8;
    while private.len() % 8 != 0 {
        private.push(pad);
        pad += 1;
    }

    let mut blob = Vec::new();
    blob.extend_from_slice(b"openssh-key-v1\0");
    put_string(&mut blob, b"none");
    put_string(&mut blob, b"none");
    put_string(&mut blob, b"");
    put_u32(&mut blob, 1);
    put_string(&mut blob, &key.public_blob);
    put_string(&mut blob, &private);

    let b64 = B64.encode(&blob);
    let mut pem = String::from("-----BEGIN OPENSSH PRIVATE KEY-----\n");
    for chunk in b64.as_bytes().chunks(70) {
        pem.push_str(std::str::from_utf8(chunk).unwrap());
        pem.push('\n');
    }
    pem.push_str("-----END OPENSSH PRIVATE KEY-----\n");
    Ok(pem)
}

/// One-shot: PPK text (+ passphrase) → russh key pair.
pub fn load(text: &str, passphrase: Option<&str>) -> Result<russh_keys::key::KeyPair> {
    let key = parse(text, passphrase)?;
    let pem = to_openssh_pem(&key)?;
    russh_keys::decode_secret_key(&pem, None).context("converted .ppk was rejected")
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh_keys::PublicKeyBase64;

    const PASS: &str = "testkey";

    fn fx(name: &str) -> String {
        std::fs::read_to_string(format!("{}/src/ppk/fixtures/{name}.ppk", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// Loads, and checks the derived public key equals the file's Public-Lines blob.
    fn load_and_check(name: &str, pass: Option<&str>) -> russh_keys::key::KeyPair {
        let text = fx(name);
        let parsed = parse(&text, pass).unwrap();
        let kp = load(&text, pass).unwrap();
        assert_eq!(kp.clone_public_key().unwrap().public_key_bytes(), parsed.public_blob, "{name}: public key mismatch");
        kp
    }

    #[test]
    fn v2_unencrypted_rsa() {
        let kp = load_and_check("ssh-rsa-v2-plain", None);
        assert_eq!(kp.name(), "rsa-sha2-512"); // russh signs RSA with SHA-512 by default
        // Passphrase is ignored for a plain file.
        load_and_check("ssh-rsa-v2-plain", Some("whatever"));
    }

    #[test]
    fn v2_encrypted_keys_of_every_supported_type() {
        assert_eq!(load_and_check("ssh-rsa-v2-enc", Some(PASS)).name(), "rsa-sha2-512");
        assert_eq!(load_and_check("ecdsa-nistp256-v2-enc", Some(PASS)).name(), "ecdsa-sha2-nistp256");
        assert_eq!(load_and_check("ecdsa-nistp384-v2-enc", Some(PASS)).name(), "ecdsa-sha2-nistp384");
        assert_eq!(load_and_check("ecdsa-nistp521-v2-enc", Some(PASS)).name(), "ecdsa-sha2-nistp521");
        let ed = load_and_check("ssh-ed25519-v2-enc", Some(PASS));
        assert_eq!(ed.name(), "ssh-ed25519");
        // Known seed from the upstream fixture: 00 d8 f7 75 …
        let parsed = parse(&fx("ssh-ed25519-v2-enc"), Some(PASS)).unwrap();
        assert_eq!(&parsed.private_blob[4..8], &[0x00, 0xd8, 0xf7, 0x75]);
        assert_eq!(parsed.comment, "a@b");
    }

    #[test]
    fn v3_argon2id() {
        let plain = load_and_check("ssh-rsa-v3-plain", None);
        let enc = load_and_check("ssh-rsa-v3-enc", Some(PASS));
        assert_eq!(plain.clone_public_key().unwrap(), enc.clone_public_key().unwrap(), "same key in both files");
    }

    #[test]
    fn wrong_passphrase_and_missing_passphrase_are_distinct_errors() {
        let e = load(&fx("ssh-rsa-v3-enc"), Some("nope")).unwrap_err().to_string();
        assert!(e.contains("wrong passphrase"), "{e}");
        let e = load(&fx("ssh-ed25519-v2-enc"), Some("nope")).unwrap_err().to_string();
        assert!(e.contains("wrong passphrase"), "{e}");
        let e = load(&fx("ssh-ed25519-v2-enc"), None).unwrap_err().to_string();
        assert!(e.contains("passphrase-protected"), "{e}");
    }

    #[test]
    fn tampering_is_caught_and_dsa_is_refused() {
        let mut text = fx("ssh-rsa-v2-plain");
        text = text.replace("Comment: a@b", "Comment: a@c");
        assert!(load(&text, None).unwrap_err().to_string().contains("corrupt"));
        let e = load(&fx("ssh-dss-v2-enc"), Some(PASS)).unwrap_err().to_string();
        assert!(e.contains("DSA"), "{e}");
        assert!(is_ppk(&fx("ssh-rsa-v2-plain")));
        assert!(!is_ppk("-----BEGIN OPENSSH PRIVATE KEY-----"));
    }
}
