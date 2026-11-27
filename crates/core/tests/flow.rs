#![cfg(feature = "native")]
//! End-to-end protocol behaviour against an in-memory authority.

use backchannel_core::authority::{Authority, BoardSpec, Mode, Profile};
use backchannel_core::crypto::{sign_envelope, verify_envelope, KeyPair};
use backchannel_core::inbox::Inbox;
use backchannel_core::sqlite::Sqlite;
use backchannel_core::tools::{call, Ctx, Me, Outcome};
use backchannel_core::*;
use serde_json::json;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
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

    let alice = Me {
        name: "alice".into(),
        keys: KeyPair::generate(),
    };
    let bob = Me {
        name: "bob".into(),
        keys: KeyPair::generate(),
    };
    auth.register("alice", &alice.keys.public_key, Profile::default())
        .unwrap();
    auth.register("bob", &bob.keys.public_key, Profile::default())
        .unwrap();

    let mut a = Ctx {
        auth: &auth,
        inbox: &inbox,
        me: &alice,
        label: "frontend".into(),
        client: "test".into(),
    };
    let mut b = Ctx {
        auth: &auth,
        inbox: &inbox,
        me: &bob,
        label: "reviewer".into(),
        client: "test".into(),
    };
    call(&mut a, "whoami", &json!({})).unwrap();
    call(&mut b, "whoami", &json!({})).unwrap();

    // Broadcast an ask. Bob follows smoko by default, so he gets it.
    let posted = text(
        call(
            &mut a,
            "post",
            &json!({ "board": "smoko", "kind": "ask", "body": "can someone review PR 42?" }),
        )
        .unwrap(),
    );
    let id = posted.split("id ").nth(1).unwrap()[..26].to_string();
    let got = text(call(&mut b, "inbox", &json!({})).unwrap());
    assert!(
        got.contains("review PR 42") && got.contains("subscribed to smoko"),
        "{got}"
    );

    // Bob claims it; Alice cannot also claim it; then Bob marks it handled.
    assert!(text(call(&mut b, "claim", &json!({ "id": id })).unwrap())
        .contains("claimed by bob/reviewer"));
    let clash = call(&mut a, "claim", &json!({ "id": id })).err().unwrap();
    assert_eq!(clash.code, Code::Conflict);
    call(&mut b, "handled", &json!({ "id": id, "note": "approved" })).unwrap();
    let h = auth.handling(&id).unwrap();
    assert_eq!(h.state, HandlingState::Handled);
    assert_eq!(h.handled_by.as_deref(), Some("bob/reviewer"));

    // Alice hears about both actions on her ask.
    let mine = text(call(&mut a, "inbox", &json!({})).unwrap());
    assert!(
        mine.contains("claim on your ask") && mine.contains("handled on your ask"),
        "{mine}"
    );

    // Direct message to a specific agent reaches only that agent.
    call(
        &mut b,
        "post",
        &json!({ "to": "alice/frontend", "body": "ship it" }),
    )
    .unwrap();
    let mut a2 = Ctx {
        auth: &auth,
        inbox: &inbox,
        me: &alice,
        label: "backend".into(),
        client: "test".into(),
    };
    call(&mut a2, "whoami", &json!({})).unwrap();
    call(
        &mut b,
        "post",
        &json!({ "to": "alice/frontend", "body": "second" }),
    )
    .unwrap();
    assert!(text(call(&mut a, "inbox", &json!({})).unwrap()).contains("second"));
    assert!(!text(call(&mut a2, "inbox", &json!({})).unwrap()).contains("second"));

    // Nothing new: wait asks the runtime to poll.
    assert_eq!(
        text(call(&mut a, "wait", &json!({ "timeout_seconds": 1 })).unwrap()),
        "<wait>"
    );
}

#[test]
fn private_boards_and_dms_stay_private() {
    let auth = Authority::new(Sqlite::memory().unwrap(), Mode::Authority, now).unwrap();
    let inbox = Inbox::new(&auth, true, vec!["**".into()]).unwrap();
    let alice = Me {
        name: "alice".into(),
        keys: KeyPair::generate(),
    };
    let carol = Me {
        name: "carol".into(),
        keys: KeyPair::generate(),
    };
    auth.register("alice", &alice.keys.public_key, Profile::default())
        .unwrap();
    auth.register("carol", &carol.keys.public_key, Profile::default())
        .unwrap();
    auth.create_board(
        "alice",
        BoardSpec {
            name: "secret".into(),
            visibility: Some(Visibility::Private),
            ..Default::default()
        },
    )
    .unwrap();

    let mut a = Ctx {
        auth: &auth,
        inbox: &inbox,
        me: &alice,
        label: "a".into(),
        client: "t".into(),
    };
    let mut c = Ctx {
        auth: &auth,
        inbox: &inbox,
        me: &carol,
        label: "c".into(),
        client: "t".into(),
    };
    call(&mut c, "whoami", &json!({})).unwrap();
    call(
        &mut a,
        "post",
        &json!({ "board": "secret", "body": "plans" }),
    )
    .unwrap();
    assert!(!text(call(&mut c, "inbox", &json!({})).unwrap()).contains("plans"));
    assert_eq!(
        call(&mut c, "post", &json!({ "board": "secret", "body": "hi" }))
            .err()
            .unwrap()
            .code,
        Code::Forbidden
    );
    assert!(auth.feed(Some("carol"), 0, 100, false).unwrap().is_empty());
}

#[test]
fn signatures_and_key_pinning() {
    let auth = Authority::new(Sqlite::memory().unwrap(), Mode::Authority, now).unwrap();
    auth.ensure_board("smoko", "").unwrap();
    let k = KeyPair::generate();
    let env = Envelope {
        v: PROTOCOL.into(),
        id: backchannel_core::crypto::ulid(now()),
        board: "smoko".into(),
        from: Author {
            name: "dave".into(),
            agent: "cli".into(),
            key: k.public_key.clone(),
        },
        kind: Kind::Note,
        body: "hello".into(),
        title: None,
        ref_: None,
        mentions: None,
        tags: None,
        data: None,
        lease: None,
        expires: None,
        ext: None,
        ts: backchannel_core::crypto::iso(now()),
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
    imp.id = backchannel_core::crypto::ulid(now());
    imp.from.key = mallory.public_key.clone();
    assert_eq!(
        auth.accept(sign_envelope(imp, &mallory))
            .err()
            .unwrap()
            .code,
        Code::Forbidden
    );

    // A replica re-verifies and pins keys.
    let replica = Authority::new(Sqlite::memory().unwrap(), Mode::Replica, now).unwrap();
    assert_eq!(
        replica.ingest(&first).unwrap(),
        backchannel_core::authority::Ingest::Stored
    );
    let mut bad = first.clone();
    bad.envelope.id = backchannel_core::crypto::ulid(now());
    assert_eq!(
        replica.ingest(&bad).unwrap(),
        backchannel_core::authority::Ingest::Rejected
    );
}

#[test]
fn the_viewer_feed_checks_each_signature() {
    use backchannel_core::sql::Sql;
    let auth = Authority::new(Sqlite::memory().unwrap(), Mode::Authority, now).unwrap();
    auth.ensure_board(DEFAULT_BOARD, "General chat").unwrap();
    let inbox = Inbox::new(&auth, true, vec![DEFAULT_BOARD.into()]).unwrap();
    let alice = Me {
        name: "alice".into(),
        keys: KeyPair::generate(),
    };
    auth.register("alice", &alice.keys.public_key, Profile::default())
        .unwrap();
    let mut a = Ctx {
        auth: &auth,
        inbox: &inbox,
        me: &alice,
        label: "a".into(),
        client: "t".into(),
    };
    call(
        &mut a,
        "post",
        &json!({ "board": "smoko", "body": "the original words" }),
    )
    .unwrap();
    call(
        &mut a,
        "post",
        &json!({ "board": "smoko", "body": "left alone" }),
    )
    .unwrap();

    let feed = backchannel_core::view::feed(&auth, 0, 100).unwrap();
    assert!(
        feed["posts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["verified"] == true),
        "{feed}"
    );

    // Someone with the database rewrites a stored post. The viewer says so.
    auth.sql.run("UPDATE posts SET envelope = replace(envelope, 'the original words', 'something else') WHERE org = 'local' AND seq = 1", &[]).unwrap();
    let feed = backchannel_core::view::feed(&auth, 0, 100).unwrap();
    let posts = feed["posts"].as_array().unwrap();
    assert_eq!(posts[0]["post"]["envelope"]["body"], "something else");
    assert_eq!(posts[0]["verified"], false);
    assert_eq!(posts[1]["verified"], true);
}

/// `data` answers the same reads as JSON, limited to what the caller may see,
/// and marks nothing read.
#[test]
fn data_gives_a_program_what_the_tools_give_an_agent() {
    let auth = Authority::new(Sqlite::memory().unwrap(), Mode::Authority, now).unwrap();
    auth.ensure_board(DEFAULT_BOARD, "General chat").unwrap();
    let inbox = Inbox::new(&auth, true, vec![DEFAULT_BOARD.into()]).unwrap();
    let people: Vec<Me> = ["alice", "bob", "carol"]
        .into_iter()
        .map(|name| {
            let me = Me {
                name: name.into(),
                keys: KeyPair::generate(),
            };
            auth.register(name, &me.keys.public_key, Profile::default())
                .unwrap();
            me
        })
        .collect();
    let ctx = |i: usize, label: &str| Ctx {
        auth: &auth,
        inbox: &inbox,
        me: &people[i],
        label: label.into(),
        client: "test".into(),
    };
    let (mut a, mut b, mut c) = (ctx(0, "desk"), ctx(1, "reviewer"), ctx(2, "ops"));
    let data = |ctx: &mut Ctx<'_, Sqlite>, args: serde_json::Value| -> serde_json::Value {
        serde_json::from_str(&text(call(ctx, "data", &args).unwrap())).unwrap()
    };
    for ctx in [&mut a, &mut b, &mut c] {
        call(ctx, "whoami", &json!({})).unwrap();
    }

    let said = text(
        call(
            &mut b,
            "post",
            &json!({ "board": "smoko", "body": "Review PR 7?", "kind": "ask" }),
        )
        .unwrap(),
    );
    let id = said
        .split("(id ")
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap()
        .to_string();
    call(
        &mut b,
        "post",
        &json!({ "to": "alice", "body": "A word in private" }),
    )
    .unwrap();
    call(
        &mut a,
        "post",
        &json!({ "to": "bob/reviewer", "body": "Go on" }),
    )
    .unwrap();

    let o = data(&mut a, json!({ "what": "overview" }));
    assert_eq!(
        (&o["org"], &o["address"]),
        (&json!("local"), &json!("alice/desk"))
    );
    assert_eq!(o["jobs"][0]["post"]["envelope"]["id"], json!(id));
    assert_eq!(o["jobs"][0]["handling"]["state"], "open");
    assert_eq!(o["inbox"].as_array().unwrap().len(), 2);
    assert_eq!(o["people"].as_array().unwrap().len(), 3);
    assert!(o["boards"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["name"] == "smoko"));
    // Looking twice shows the same inbox: nothing was marked read.
    assert_eq!(
        data(&mut a, json!({ "what": "overview" }))["inbox"],
        o["inbox"]
    );

    call(&mut a, "claim", &json!({ "id": id })).unwrap();
    let t = data(&mut c, json!({ "what": "thread", "id": id }));
    assert_eq!(t["handling"]["holder"], "alice/desk");
    assert_eq!(t["posts"].as_array().unwrap().len(), 2);
    // A claim carries the state of the post it is about.
    assert_eq!(t["posts"][1]["handling"]["post"], json!(id));
    assert_eq!(
        data(&mut c, json!({ "what": "board", "name": "smoko" }))["posts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Direct messages: both sides of the conversation, and nobody else's.
    let bodies = |v: &serde_json::Value| -> Vec<String> {
        v["posts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["post"]["envelope"]["body"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        bodies(&data(&mut a, json!({ "what": "direct", "with": "bob" }))),
        ["A word in private", "Go on"]
    );
    assert_eq!(
        bodies(&data(
            &mut a,
            json!({ "what": "direct", "with": "bob/reviewer" })
        )),
        ["A word in private", "Go on"]
    );
    assert_eq!(
        bodies(&data(&mut b, json!({ "what": "direct", "with": "alice" }))),
        ["A word in private", "Go on"]
    );
    assert!(bodies(&data(&mut c, json!({ "what": "direct", "with": "alice" }))).is_empty());
    assert!(bodies(&data(&mut c, json!({ "what": "direct", "with": "bob" }))).is_empty());

    assert!(call(&mut a, "data", &json!({ "what": "everything" })).is_err());
    assert!(call(
        &mut a,
        "data",
        &json!({ "what": "direct", "with": "nobody" })
    )
    .is_err());
}
