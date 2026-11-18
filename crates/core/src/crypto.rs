//! Canonical JSON, Ed25519 signing, request auth, ids and time helpers.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::types::{Envelope, PROTOCOL};

/// Canonical JSON: object keys sorted by UTF-16 code unit, no insignificant
/// whitespace. Matches RFC 8785 (JCS) for strings, integers and objects.
/// Prefer integers in `data`; floats are serialized with Rust's shortest
/// round-trip form, which can differ from JCS for some values.
pub fn canonical(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap());
                out.push(':');
                write_canonical(&m[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap()),
    }
}

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(s: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).ok()
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("system randomness unavailable");
    b
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyPair {
    /// base64url of the 32-byte public key.
    pub public_key: String,
    /// base64url of the 32-byte secret seed.
    pub secret_key: String,
}

impl KeyPair {
    pub fn generate() -> KeyPair {
        KeyPair::from_seed(&random_bytes::<32>())
    }

    pub fn from_seed(seed: &[u8; 32]) -> KeyPair {
        let sk = SigningKey::from_bytes(seed);
        KeyPair { public_key: b64(sk.verifying_key().as_bytes()), secret_key: b64(seed) }
    }

    /// Accepts a base64url seed (as stored in a relay secret).
    pub fn from_seed_b64(s: &str) -> Option<KeyPair> {
        let bytes: [u8; 32] = unb64(s.trim())?.try_into().ok()?;
        Some(KeyPair::from_seed(&bytes))
    }

    fn signing_key(&self) -> SigningKey {
        let seed: [u8; 32] = unb64(&self.secret_key).and_then(|b| b.try_into().ok()).expect("corrupt secret key");
        SigningKey::from_bytes(&seed)
    }

    pub fn sign(&self, msg: &[u8]) -> String {
        b64(&self.signing_key().sign(msg).to_bytes())
    }
}

pub fn verify(public_key: &str, msg: &[u8], sig: &str) -> bool {
    let Some(pk) = unb64(public_key).and_then(|b| <[u8; 32]>::try_from(b).ok()) else { return false };
    let Some(sig) = unb64(sig).and_then(|b| <[u8; 64]>::try_from(b).ok()) else { return false };
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else { return false };
    vk.verify_strict(msg, &Signature::from_bytes(&sig)).is_ok()
}

/// Bytes an envelope signature covers. The prefix stops a signature from being
/// replayed in another context (or another protocol version).
pub fn signing_bytes(env: &Envelope) -> Vec<u8> {
    let mut v = serde_json::to_value(env).expect("envelope serializes");
    v.as_object_mut().unwrap().remove("sig");
    format!("{PROTOCOL}\n{}", canonical(&v)).into_bytes()
}

pub fn sign_envelope(mut env: Envelope, keys: &KeyPair) -> Envelope {
    env.sig = String::new();
    env.sig = keys.sign(&signing_bytes(&env));
    env
}

pub fn verify_envelope(env: &Envelope) -> bool {
    verify(&env.from.key, &signing_bytes(env), &env.sig)
}

pub fn sha256_b64(data: &[u8]) -> String {
    b64(&Sha256::digest(data))
}

/// Short, human-checkable key fingerprint, e.g. "3fa1-09c2-77de".
pub fn fingerprint(public_key: &str) -> String {
    let raw = unb64(public_key).unwrap_or_default();
    let d = Sha256::digest(&raw);
    let hex: String = d[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}", &hex[0..4], &hex[4..8], &hex[8..12])
}

// ---- request auth -----------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthClaims {
    pub name: String,
    pub key: String,
    pub method: String,
    /// Path plus query string, exactly as sent.
    pub path: String,
    pub ts: String,
    /// sha256 (base64url) of the raw request body; of "" when there is none.
    pub body: String,
}

const AUTH_PREFIX: &str = "backchannel-auth/0.1\n";

/// Builds an `Authorization: Backchannel <claims>.<sig>` header value.
pub fn sign_request(name: &str, keys: &KeyPair, method: &str, path: &str, body: &[u8], now_ms: i64) -> String {
    let claims = AuthClaims {
        name: name.to_string(),
        key: keys.public_key.clone(),
        method: method.to_uppercase(),
        path: path.to_string(),
        ts: iso(now_ms),
        body: sha256_b64(body),
    };
    let c = canonical(&serde_json::to_value(&claims).unwrap());
    format!("Backchannel {}.{}", b64(c.as_bytes()), keys.sign(format!("{AUTH_PREFIX}{c}").as_bytes()))
}

/// Verifies signature, method, path, body hash and freshness. Key binding is the caller's job.
pub fn verify_request(header: &str, method: &str, path: &str, body: &[u8], now_ms: i64) -> Option<AuthClaims> {
    let rest = header.strip_prefix("Backchannel ")?;
    let (c64, sig) = rest.split_once('.')?;
    let c = String::from_utf8(unb64(c64)?).ok()?;
    let claims: AuthClaims = serde_json::from_str(&c).ok()?;
    if claims.method != method.to_uppercase() || claims.path != path || claims.body != sha256_b64(body) {
        return None;
    }
    if (parse_iso(&claims.ts)? - now_ms).abs() > crate::types::limits::CLOCK_SKEW_MS {
        return None;
    }
    let recanon = canonical(&serde_json::to_value(&claims).ok()?);
    verify(&claims.key, format!("{AUTH_PREFIX}{recanon}").as_bytes(), sig).then_some(claims)
}

// ---- ids and time -----------------------------------------------------------

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A ULID: 48-bit millisecond time + 80 random bits, Crockford base32.
pub fn ulid(now_ms: i64) -> String {
    let mut out = [0u8; 26];
    let mut t = now_ms as u64;
    for i in (0..10).rev() {
        out[i] = CROCKFORD[(t % 32) as usize];
        t /= 32;
    }
    let rnd = random_bytes::<16>();
    for i in 0..16 {
        out[10 + i] = CROCKFORD[(rnd[i] % 32) as usize];
    }
    String::from_utf8(out.to_vec()).unwrap()
}

pub fn is_ulid(s: &str) -> bool {
    s.len() == 26 && s.bytes().all(|b| CROCKFORD.contains(&b))
}

pub fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn parse_iso(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis())
}
