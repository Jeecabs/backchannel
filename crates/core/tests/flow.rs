#![cfg(feature = "native")]
//! End-to-end protocol behaviour against an in-memory authority.

use serde_json::json;
use smoko_core::authority::{Authority, BoardSpec, Mode, Profile};
use smoko_core::crypto::{sign_envelope, verify_envelope, KeyPair};
use smoko_core::inbox::Inbox;
use smoko_core::sqlite::Sqlite;
use smoko_core::tools::{call, Ctx, Me, Outcome};
use smoko_core::*;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

fn text(o: Option<Outcome>) -> String {
    match o {
        Some(Outcome::Text(t)) => t,
        Some(Outcome::Wait { .. }) => "<wait>".into(),
        None => "<unknown tool>".into(),
    }
}

#[test]
fn two_people_talk_and_hand_off_work() {
    let auth = Authority::new(Sqlite::memory().unwrap(), Mode::Authority, now).unwrap();
    auth.ensure_board(DEFAULT_BOARD, "General chat").unwrap();
    let inbox = Inbox::new(&auth, true, vec![DEFAULT_BOARD.into()]).unwrap();

    let alice = Me { name: "alice".into(), keys: KeyPair::generate() };
    let bob = Me { name: "bob".into(), keys: KeyPair::generate() };
    auth.register("alice", &alice.keys.public_key, Profile::default()).unwrap();
    auth.register("bob", &bob.keys.public_key, Profile::default()).unwrap();

    let mut a = Ctx { auth: &auth, inbox: &inbox, me: &alice, label: "frontend".into(), client: "test".into() };
    let mut b = Ctx { auth: &auth, inbox: &inbox, me: &bob, label: "reviewer".into(), client: "test".into() };
    call(&mut a, "whoami", &json!({})).unwrap();
    call(&mut b, "whoami", &json!({})).unwrap();

    // Broadcast an ask. Bob follows the smoko by default, so he gets it.
    let posted = text(call(&mut a, "post", &json!({ "board": "smoko", "kind": "ask", "body": "can someone review PR 42?" })).unwrap());
    let id = posted.split("id ").nth(1).unwrap()[..26].to_string();
    let got = text(call(&mut b, "inbox", &json!({})).unwrap());
    assert!(got.contains("review PR 42") && got.contains("subscribed to smoko"), "{got}");

    // Bob claims it; Alice cannot also claim it; then Bob marks it handled.
    assert!(text(call(&mut b, "claim", &json!({ "id": id })).unwrap()).contains("claimed by bob/reviewer"));
    let clash = call(&mut a, "claim", &json!({ "id": id })).err().unwrap();
    assert_eq!(clash.code, Code::Conflict);
    call(&mut b, "handled", &json!({ "id": id, "note": "approved" })).unwrap();
    let h = auth.handling(&id).unwrap();
    assert_eq!(h.state, HandlingState::Handled);
    assert_eq!(h.handled_by.as_deref(), Some("bob/reviewer"));

    // Alice hears about both actions on her ask.
    let mine = text(call(&mut a, "inbox", &json!({})).unwrap());
    assert!(mine.contains("claim on your ask") && mine.contains("handled on your ask"), "{mine}");

    // Direct message to a specific agent reaches only that agent.
    call(&mut b, "post", &json!({ "to": "alice/frontend", "body": "ship it" })).unwrap();
    let mut a2 = Ctx { auth: &auth, inbox: &inbox, me: &alice, label: "backend".into(), client: "test".into() };
    call(&mut a2, "whoami", &json!({})).unwrap();
    call(&mut b, "post", &json!({ "to": "alice/frontend", "body": "second" })).unwrap();
    assert!(text(call(&mut a, "inbox", &json!({})).unwrap()).contains("second"));
    assert!(!text(call(&mut a2, "inbox", &json!({})).unwrap()).contains("second"));

    // Nothing new: wait asks the runtime to poll.
    assert_eq!(text(call(&mut a, "wait", &json!({ "timeout_seconds": 1 })).unwrap()), "<wait>");
}

#[test]
fn private_boards_and_dms_stay_private() {
    let auth = Authority::new(Sqlite::memory().unwrap(), Mode::Authority, now).unwrap();
    let inbox = Inbox::new(&auth, true, vec!["**".into()]).unwrap();
    let alice = Me { name: "alice".into(), keys: KeyPair::generate() };
    let carol = Me { name: "carol".into(), keys: KeyPair::generate() };
    auth.register("alice", &alice.keys.public_key, Profile::default()).unwrap();
    auth.register("carol", &carol.keys.public_key, Profile::default()).unwrap();
    auth.create_board("alice", BoardSpec { name: "secret".into(), visibility: Some(Visibility::Private), ..Default::default() }).unwrap();

    let mut a = Ctx { auth: &auth, inbox: &inbox, me: &alice, label: "a".into(), client: "t".into() };
    let mut c = Ctx { auth: &auth, inbox: &inbox, me: &carol, label: "c".into(), client: "t".into() };
    call(&mut c, "whoami", &json!({})).unwrap();
    call(&mut a, "post", &json!({ "board": "secret", "body": "plans" })).unwrap();
    assert!(!text(call(&mut c, "inbox", &json!({})).unwrap()).contains("plans"));
    assert_eq!(call(&mut c, "post", &json!({ "board": "secret", "body": "hi" })).err().unwrap().code, Code::Forbidden);
    assert!(auth.feed(Some("carol"), 0, 100, false).unwrap().is_empty());
}

#[test]
fn signatures_and_key_pinning() {
    let auth = Authority::new(Sqlite::memory().unwrap(), Mode::Authority, now).unwrap();
    auth.ensure_board("smoko", "").unwrap();
    let k = KeyPair::generate();
    let env = Envelope {
        v: PROTOCOL.into(),
        id: smoko_core::crypto::ulid(now()),
        board: "smoko".into(),
        from: Author { name: "dave".into(), agent: "cli".into(), key: k.public_key.clone() },
        kind: Kind::Note,
        body: "hello".into(),
        title: None, ref_: None, mentions: None, tags: None, data: None, lease: None, expires: None, ext: None,
        ts: smoko_core::crypto::iso(now()),
        sig: String::new(),
    };
    let signed = sign_envelope(env.clone(), &k);
    assert!(verify_envelope(&signed));

    // Tampering breaks the signature.
    let mut forged = signed.clone();
    forged.body = "goodbye".into();
    assert_eq!(auth.accept(forged).err().unwrap().code, Code::BadSignature);

    // Accepting is idempotent on id.
    let first = auth.accept(signed.clone()).unwrap();
    assert_eq!(auth.accept(signed).unwrap().gseq, first.gseq);

    // Someone else's key cannot post as dave.
    let mallory = KeyPair::generate();
    let mut imp = env;
    imp.id = smoko_core::crypto::ulid(now());
    imp.from.key = mallory.public_key.clone();
    assert_eq!(auth.accept(sign_envelope(imp, &mallory)).err().unwrap().code, Code::Forbidden);

    // A replica re-verifies and pins keys.
    let replica = Authority::new(Sqlite::memory().unwrap(), Mode::Replica, now).unwrap();
    assert_eq!(replica.ingest(&first).unwrap(), smoko_core::authority::Ingest::Stored);
    let mut bad = first.clone();
    bad.envelope.id = smoko_core::crypto::ulid(now());
    assert_eq!(replica.ingest(&bad).unwrap(), smoko_core::authority::Ingest::Rejected);
}
