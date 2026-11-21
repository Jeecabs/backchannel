#![cfg(feature = "native")]
//! What 0.1.0 wrote must keep working: key files load, signatures verify,
//! and hashes of tokens and invite codes come out the same.
//!
//! `fixtures/made-by-0.1.0.json` was produced by the code at the v0.1.0 tag
//! (ed25519-dalek 2.2.0, sha2 0.10.9, base64 0.22.1) and is never regenerated.
//! A dependency upgrade that changes any of this would sign every member out of
//! a live relay, or stop their old posts verifying, so it must fail here first.

use backchannel_core::authority::{Authority, Ingest, Mode};
use backchannel_core::crypto::{
    fingerprint, sha256_b64, sign_envelope, sign_request, verify_envelope, verify_request, KeyPair,
};
use backchannel_core::sqlite::Sqlite;
use backchannel_core::{tokens, Envelope};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/made-by-0.1.0.json")).unwrap()
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap()
}

#[test]
fn a_key_file_from_0_1_0_loads_and_is_the_same_key() {
    let f = fixture();
    // The file exactly as `backchannel init` wrote it to ~/.backchannel/identity.key.
    let keys: KeyPair = serde_json::from_str(text(&f["key_file"])).unwrap();
    let envelope: Envelope = serde_json::from_value(f["envelope"].clone()).unwrap();
    assert_eq!(keys.public_key, envelope.from.key);
    // The public key is derived from the stored seed the same way.
    assert_eq!(
        KeyPair::from_seed_b64(&keys.secret_key).unwrap().public_key,
        keys.public_key
    );
    assert_eq!(fingerprint(&keys.public_key), text(&f["fingerprint"]));
    // Writing it back gives the same file.
    assert_eq!(
        serde_json::to_string_pretty(&keys).unwrap() + "\n",
        text(&f["key_file"])
    );
}

#[test]
fn a_post_signed_by_0_1_0_still_verifies_and_signing_again_gives_the_same_signature() {
    let f = fixture();
    let keys: KeyPair = serde_json::from_str(text(&f["key_file"])).unwrap();
    let envelope: Envelope = serde_json::from_value(f["envelope"].clone()).unwrap();
    assert!(
        verify_envelope(&envelope),
        "a signature made by 0.1.0 no longer verifies"
    );

    // Ed25519 signatures are deterministic: the same key over the same bytes
    // must give the same signature, or the bytes or the key changed.
    let again = sign_envelope(envelope.clone(), &keys);
    assert_eq!(again.sig, envelope.sig);

    // Any change still breaks it.
    let mut edited = envelope.clone();
    edited.body.push('!');
    assert!(!verify_envelope(&edited));
    let mut other_key = envelope.clone();
    other_key.from.key = KeyPair::generate().public_key;
    assert!(!verify_envelope(&other_key));

    // An authority accepts it, and a replica that re-verifies stores it.
    let at = backchannel_core::crypto::parse_iso(&envelope.ts).unwrap();
    let auth = Authority::new(Sqlite::memory().unwrap(), Mode::Authority, move || at).unwrap();
    auth.ensure_board("ops/deploys", "").unwrap();
    let stored = auth.accept(envelope.clone()).unwrap();
    assert_eq!(stored.envelope.sig, envelope.sig);
    let replica = Authority::new(Sqlite::memory().unwrap(), Mode::Replica, move || at).unwrap();
    assert_eq!(replica.ingest(&stored).unwrap(), Ingest::Stored);
}

#[test]
fn a_request_signed_by_0_1_0_still_verifies() {
    let f = fixture();
    let keys: KeyPair = serde_json::from_str(text(&f["key_file"])).unwrap();
    let r = &f["request"];
    let (header, method, path, body, now) = (
        text(&r["header"]),
        text(&r["method"]),
        text(&r["path"]),
        text(&r["body"]).as_bytes(),
        r["now_ms"].as_i64().unwrap(),
    );
    let claims = verify_request(header, method, path, body, now)
        .expect("a request signed by 0.1.0 no longer verifies");
    assert_eq!(
        (claims.name.as_str(), claims.key.as_str()),
        (text(&r["name"]), keys.public_key.as_str())
    );
    assert_eq!(
        sign_request(text(&r["name"]), &keys, method, path, body, now),
        header
    );
    assert!(verify_request(header, method, path, b"another body", now).is_none());
}

#[test]
fn tokens_and_invites_hash_to_what_0_1_0_stored() {
    let f = fixture();
    // A relay finds a token by this hash. If it came out differently, every
    // token already handed out would stop working.
    assert_eq!(tokens::hash(text(&f["token"])), text(&f["token_hash"]));
    assert_eq!(
        sha256_b64(text(&f["token"]).as_bytes()),
        text(&f["token_hash"])
    );
    assert_eq!(
        sha256_b64(text(&f["invite_code"]).as_bytes()),
        text(&f["invite_hash"])
    );
    // The short id shown for a token is a function of that hash and nothing else.
    assert_eq!(
        tokens::id_of(text(&f["token"])),
        tokens::id_of_hash(text(&f["token_hash"]))
    );

    // And a tokens table that 0.1.0 filled lets the same token in.
    use backchannel_core::sql::Sql;
    let sql = Sqlite::memory().unwrap();
    sql.batch("CREATE TABLE tokens (hash TEXT PRIMARY KEY, org TEXT NOT NULL, name TEXT NOT NULL, created_at INTEGER NOT NULL, last_used INTEGER)").unwrap();
    sql.run(
        "INSERT INTO tokens (hash, org, name, created_at) VALUES (?, 'acme', 'lachlan', 1)",
        backchannel_core::p![text(&f["token_hash"])],
    )
    .unwrap();
    tokens::migrate(&sql).unwrap();
    let session = tokens::authenticate(&sql, text(&f["token"]), "test", 2)
        .unwrap()
        .expect("a 0.1.0 token no longer signs in");
    assert_eq!(
        (session.org.as_str(), session.name.as_str()),
        ("acme", "lachlan")
    );
}
