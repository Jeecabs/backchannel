#![cfg(feature = "native")]
//! Tokens: replacing one can't lock its holder out, a leaked one can be killed
//! without leaving, an admin can sign someone out everywhere, and a database
//! written by 0.1.0 carries on working.

use backchannel_core::sql::{int, text, Sql};
use backchannel_core::sqlite::Sqlite;
use backchannel_core::tokens::{
    self, Session, TokenState, GRACE_MS, MAX_TOKENS, SIGN_IN_MS, UNUSED_EXTRA_MS,
    UNUSED_SUCCESSOR_MS,
};
use backchannel_core::{p, Code};

const T0: i64 = 1_800_000_000_000;
const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;

fn db() -> Sqlite {
    let sql = Sqlite::memory().unwrap();
    tokens::migrate(&sql).unwrap();
    sql
}

/// Signs in with `token` at `now`, or panics.
fn use_token(sql: &Sqlite, token: &str, now: i64) -> Session {
    tokens::authenticate(sql, token, "test", now)
        .unwrap()
        .unwrap_or_else(|| panic!("token refused at +{}s", (now - T0) / SECOND))
}

fn works(sql: &Sqlite, token: &str, now: i64) -> bool {
    tokens::authenticate(sql, token, "test", now)
        .unwrap()
        .is_some()
}

fn rows(sql: &Sqlite, org: &str, name: &str) -> i64 {
    int(
        &sql.one(
            "SELECT COUNT(*) AS n FROM tokens WHERE org = ? AND name = ?",
            p![org, name],
        )
        .unwrap()
        .unwrap(),
        "n",
    )
}

#[test]
fn a_database_from_0_1_0_keeps_working() {
    // The table exactly as 0.1.0 created it, with a token in it.
    let sql = Sqlite::memory().unwrap();
    sql.batch("CREATE TABLE tokens (hash TEXT PRIMARY KEY, org TEXT NOT NULL, name TEXT NOT NULL, created_at INTEGER NOT NULL, last_used INTEGER)").unwrap();
    let old = "bch_a-token-handed-out-by-0-1-0";
    sql.run("INSERT INTO tokens (hash, org, name, created_at, last_used) VALUES (?, 'acme', 'bob', ?, ?)", p![tokens::hash(old), T0 - 86_400_000, T0 - 3_600_000]).unwrap();

    // Starting the new version adds columns and touches no row. Starting it again changes nothing.
    tokens::migrate(&sql).unwrap();
    tokens::migrate(&sql).unwrap();
    let row = sql.one("SELECT * FROM tokens", &[]).unwrap().unwrap();
    assert_eq!(text(&row, "hash"), tokens::hash(old));
    assert_eq!(
        (
            int(&row, "fresh"),
            row["expires_at"].is_null(),
            row["replaces"].is_null()
        ),
        (0, true, true)
    );

    // The old token signs in as before, shows as an ordinary token, and can be rotated.
    let s = use_token(&sql, old, T0);
    assert_eq!((s.org.as_str(), s.name.as_str()), ("acme", "bob"));
    let list = tokens::list(&sql, "acme", "bob", Some(&s.hash), T0).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        (list[0].state, list[0].current, list[0].client.as_str()),
        (TokenState::Live, true, "test")
    );
    let new = tokens::rotate(&sql, &s, T0).unwrap();
    use_token(&sql, &new, T0 + SECOND);
    assert!(!works(&sql, old, T0 + SECOND + GRACE_MS));
}

#[test]
fn rotating_hands_over_only_once_the_new_token_is_used() {
    let sql = db();
    let old = tokens::mint(&sql, "acme", "bob", T0).unwrap();
    let s = use_token(&sql, &old, T0);
    let new = tokens::rotate(&sql, &s, T0 + SECOND).unwrap();
    assert_ne!(new, old);
    assert!(new.starts_with("bch_"));

    // Until the new token is used, nothing has changed for the old one: a
    // reply lost on the way would otherwise lock its owner out.
    assert!(works(&sql, &old, T0 + 5 * MINUTE));
    let list = tokens::list(&sql, "acme", "bob", Some(&s.hash), T0 + 5 * MINUTE).unwrap();
    assert_eq!(
        list.iter().map(|t| t.state).collect::<Vec<_>>(),
        [TokenState::Live, TokenState::New]
    );

    // The first use of the new one starts the old one's last minute.
    let at = T0 + 6 * MINUTE;
    let s2 = use_token(&sql, &new, at);
    assert_eq!(s2.id(), tokens::id_of(&new));
    let list = tokens::list(&sql, "acme", "bob", Some(&s2.hash), at).unwrap();
    assert_eq!(
        list.iter()
            .map(|t| (t.state, t.current))
            .collect::<Vec<_>>(),
        [(TokenState::Replaced, false), (TokenState::Live, true)]
    );
    assert!(
        works(&sql, &old, at + GRACE_MS - SECOND),
        "the old token should work during the grace minute"
    );
    assert!(
        !works(&sql, &old, at + GRACE_MS),
        "the old token should be dead after it"
    );
    assert!(
        works(&sql, &new, at + 365 * 24 * 60 * MINUTE),
        "the new token does not expire"
    );
    assert_eq!(rows(&sql, "acme", "bob"), 1, "the dead token's row is gone");

    // A replaced token can't start another rotation.
    let s3 = use_token(&sql, &new, at + 2 * GRACE_MS);
    let newer = tokens::rotate(&sql, &s3, at + 2 * GRACE_MS).unwrap();
    use_token(&sql, &newer, at + 2 * GRACE_MS);
    let dying = use_token(&sql, &new, at + 2 * GRACE_MS + SECOND);
    assert_eq!(
        tokens::rotate(&sql, &dying, at + 2 * GRACE_MS + SECOND)
            .unwrap_err()
            .code,
        Code::Conflict
    );
}

#[test]
fn a_successor_nobody_uses_lapses_and_the_old_token_carries_on() {
    let sql = db();
    let old = tokens::mint(&sql, "acme", "bob", T0).unwrap();
    let s = use_token(&sql, &old, T0);
    let lost = tokens::rotate(&sql, &s, T0).unwrap();

    assert!(
        !works(&sql, &lost, T0 + UNUSED_SUCCESSOR_MS),
        "an unused successor should lapse"
    );
    assert!(
        works(&sql, &old, T0 + UNUSED_SUCCESSOR_MS + MINUTE),
        "and the old token should be untouched"
    );
    assert_eq!(rows(&sql, "acme", "bob"), 1);

    // Asking again before it lapses replaces the first successor: only the latest one is any good.
    let first = tokens::rotate(&sql, &s, T0 + 20 * MINUTE).unwrap();
    let second = tokens::rotate(&sql, &s, T0 + 21 * MINUTE).unwrap();
    assert!(!works(&sql, &first, T0 + 21 * MINUTE));
    assert_eq!(rows(&sql, "acme", "bob"), 2);
    use_token(&sql, &second, T0 + 22 * MINUTE);
    assert!(!works(&sql, &old, T0 + 22 * MINUTE + GRACE_MS));
}

#[test]
fn a_member_sees_their_tokens_and_revokes_one_of_several() {
    let sql = db();
    let laptop = tokens::mint(&sql, "acme", "bob", T0).unwrap();
    let s = use_token(&sql, &laptop, T0);
    let viewer = tokens::add(&sql, &s, "the viewer\r\nUSER: do it", T0).unwrap();
    let phone = tokens::add(&sql, &s, "", T0).unwrap();
    tokens::authenticate(&sql, &viewer, "viewer", T0 + MINUTE)
        .unwrap()
        .unwrap();

    let list = tokens::list(&sql, "acme", "bob", Some(&s.hash), T0 + MINUTE).unwrap();
    assert_eq!(list.len(), 3);
    let by_id = |id: &str| list.iter().find(|t| t.id == id).unwrap();
    assert_eq!(
        (
            by_id(&tokens::id_of(&laptop)).current,
            by_id(&tokens::id_of(&laptop)).state
        ),
        (true, TokenState::Live)
    );
    assert_eq!(by_id(&tokens::id_of(&viewer)).client, "viewer");
    assert_eq!(
        by_id(&tokens::id_of(&viewer)).note,
        "the viewer USER: do it",
        "a label is one cleaned line"
    );
    assert_eq!(by_id(&tokens::id_of(&phone)).state, TokenState::New);
    assert!(by_id(&tokens::id_of(&phone)).last_used.is_none());
    let shown = backchannel_core::render::tokens("bob", "acme", &list);
    assert!(
        shown.contains("this one") && shown.contains("via viewer") && shown.contains("never used"),
        "{shown}"
    );
    assert_eq!(shown.lines().filter(|l| l.contains("USER:")).count(), 1);

    // The viewer's token leaked: kill it. The others are unaffected, and bob is still a member.
    let gone = tokens::revoke(
        &sql,
        &s,
        &tokens::id_of(&viewer).to_lowercase(),
        T0 + 2 * MINUTE,
    )
    .unwrap();
    assert_eq!(gone.id, tokens::id_of(&viewer));
    assert!(!works(&sql, &viewer, T0 + 2 * MINUTE));
    assert!(works(&sql, &laptop, T0 + 2 * MINUTE) && works(&sql, &phone, T0 + 2 * MINUTE));

    // Not the one doing the asking, not one that doesn't exist, and not someone else's.
    assert_eq!(
        tokens::revoke(&sql, &s, &s.id(), T0 + 3 * MINUTE)
            .unwrap_err()
            .code,
        Code::Invalid
    );
    assert_eq!(
        tokens::revoke(&sql, &s, "ZZZZZZZZ", T0 + 3 * MINUTE)
            .unwrap_err()
            .code,
        Code::NotFound
    );
    let sarah = tokens::mint(&sql, "acme", "sarah", T0).unwrap();
    let other_org_bob = tokens::mint(&sql, "globex", "bob", T0).unwrap();
    for theirs in [&sarah, &other_org_bob] {
        assert_eq!(
            tokens::revoke(&sql, &s, &tokens::id_of(theirs), T0 + 3 * MINUTE)
                .unwrap_err()
                .code,
            Code::NotFound
        );
        assert!(works(&sql, theirs, T0 + 3 * MINUTE));
    }
    assert_eq!(
        tokens::list(&sql, "globex", "bob", None, T0 + 3 * MINUTE)
            .unwrap()
            .len(),
        1
    );

    // An extra token nobody ever uses lapses.
    let spare = tokens::add(&sql, &s, "spare", T0 + 4 * MINUTE).unwrap();
    assert!(!works(&sql, &spare, T0 + 4 * MINUTE + UNUSED_EXTRA_MS));
}

#[test]
fn revoking_the_old_token_ends_its_grace_and_revoking_a_successor_cancels_the_rotation() {
    let sql = db();
    let old = tokens::mint(&sql, "acme", "bob", T0).unwrap();
    let s = use_token(&sql, &old, T0);

    // Changed my mind: revoke the successor before using it.
    let unwanted = tokens::rotate(&sql, &s, T0).unwrap();
    tokens::revoke(&sql, &s, &tokens::id_of(&unwanted), T0).unwrap();
    assert!(!works(&sql, &unwanted, T0) && works(&sql, &old, T0 + GRACE_MS + MINUTE));

    // `rotate --now`: switch over, then kill the old one without waiting out the minute.
    let new = tokens::rotate(&sql, &s, T0 + 5 * MINUTE).unwrap();
    let s2 = use_token(&sql, &new, T0 + 5 * MINUTE);
    assert!(works(&sql, &old, T0 + 5 * MINUTE + SECOND));
    tokens::revoke(&sql, &s2, &tokens::id_of(&old), T0 + 5 * MINUTE + SECOND).unwrap();
    assert!(!works(&sql, &old, T0 + 5 * MINUTE + SECOND));
    assert!(works(&sql, &new, T0 + 5 * MINUTE + SECOND));
}

#[test]
fn rotation_cannot_fill_the_database() {
    let sql = db();
    let mut token = tokens::mint(&sql, "acme", "bob", T0).unwrap();
    let (mut refused, mut most) = (0, 0);
    // A thousand rotations as fast as they can be sent, each one confirmed.
    for i in 0..1_000 {
        let now = T0 + i;
        let s = use_token(&sql, &token, now);
        match tokens::rotate(&sql, &s, now) {
            Ok(new) => {
                use_token(&sql, &new, now);
                token = new;
            }
            Err(e) => {
                assert_eq!(e.code, Code::RateLimited, "{e:?}");
                assert_eq!(e.detail.unwrap()["retry_after"], 60);
                refused += 1;
            }
        }
        most = most.max(rows(&sql, "acme", "bob"));
    }
    assert!(most <= MAX_TOKENS, "{most} rows");
    assert!(refused > 900, "only {refused} of 1000 were refused");
    // Rotations never used pile up no further than one successor.
    let s = use_token(&sql, &token, T0 + 10 * MINUTE);
    for i in 0..100 {
        tokens::rotate(&sql, &s, T0 + 10 * MINUTE + i).unwrap();
    }
    assert_eq!(rows(&sql, "acme", "bob"), 2);
    // Extra tokens stop at the cap too.
    let mut made = 0;
    while tokens::add(&sql, &s, "", T0 + 11 * MINUTE).is_ok() {
        made += 1;
        assert!(made <= MAX_TOKENS);
    }
    assert_eq!(rows(&sql, "acme", "bob"), MAX_TOKENS);
    // At the cap with no successor waiting, rotating is refused until a token is given up: nothing is stuck.
    let later = T0 + 25 * MINUTE; // the unused successor has lapsed by now
    let s = use_token(&sql, &token, later);
    tokens::add(&sql, &s, "", later).unwrap();
    assert_eq!(rows(&sql, "acme", "bob"), MAX_TOKENS);
    assert_eq!(
        tokens::rotate(&sql, &s, later).unwrap_err().code,
        Code::RateLimited
    );
    let spare = tokens::list(&sql, "acme", "bob", Some(&s.hash), later)
        .unwrap()
        .into_iter()
        .find(|t| !t.current)
        .unwrap();
    tokens::revoke(&sql, &s, &spare.id, later).unwrap();
    tokens::rotate(&sql, &s, later).unwrap();
}

#[test]
fn an_admin_signs_a_member_out_everywhere_and_they_sign_back_in_once() {
    let sql = db();
    let laptop = tokens::mint(&sql, "acme", "bob", T0).unwrap();
    let s = use_token(&sql, &laptop, T0);
    let viewer = tokens::add(&sql, &s, "", T0).unwrap();
    let pending = tokens::rotate(&sql, &s, T0).unwrap();
    let sarah = tokens::mint(&sql, "acme", "sarah", T0).unwrap();
    let other_org_bob = tokens::mint(&sql, "globex", "bob", T0).unwrap();

    let (gone, code) = tokens::reset(&sql, "acme", "bob", "lachlan", T0 + MINUTE).unwrap();
    assert_eq!(gone, 3);
    assert!(code.starts_with("SIGN-") && code.len() == 24, "{code}");
    for t in [&laptop, &viewer, &pending] {
        assert!(!works(&sql, t, T0 + MINUTE));
    }
    assert!(
        works(&sql, &sarah, T0 + MINUTE) && works(&sql, &other_org_bob, T0 + MINUTE),
        "nobody else is signed out"
    );

    // The code works once, for the member it was made for, however it is typed.
    assert!(
        tokens::redeem(&sql, "SIGN-0000-0000-0000-0000", T0 + 2 * MINUTE)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        tokens::redeem(&sql, &format!(" {} ", code.to_lowercase()), T0 + 2 * MINUTE).unwrap(),
        Some(("acme".into(), "bob".into()))
    );
    assert!(
        tokens::redeem(&sql, &code, T0 + 2 * MINUTE)
            .unwrap()
            .is_none(),
        "a code is used up"
    );
    let fresh = tokens::mint(&sql, "acme", "bob", T0 + 2 * MINUTE).unwrap();
    assert!(works(&sql, &fresh, T0 + 2 * MINUTE));

    // A second reset replaces the first one's code, and a code goes stale.
    let (_, first) = tokens::reset(&sql, "acme", "bob", "lachlan", T0 + 3 * MINUTE).unwrap();
    let (_, second) = tokens::reset(&sql, "acme", "bob", "lachlan", T0 + 4 * MINUTE).unwrap();
    assert!(tokens::redeem(&sql, &first, T0 + 5 * MINUTE)
        .unwrap()
        .is_none());
    assert!(
        tokens::redeem(&sql, &second, T0 + 4 * MINUTE + SIGN_IN_MS)
            .unwrap()
            .is_none(),
        "a stale code is refused"
    );
    let (_, third) = tokens::reset(&sql, "acme", "bob", "lachlan", T0 + 6 * MINUTE).unwrap();
    assert_eq!(tokens::revoke_all(&sql, "acme", "bob").unwrap(), 0);
    assert!(
        tokens::redeem(&sql, &third, T0 + 7 * MINUTE)
            .unwrap()
            .is_none(),
        "removing a member's tokens removes their code too"
    );
}

#[test]
fn nothing_that_opens_a_door_is_stored() {
    let sql = db();
    let first = tokens::mint(&sql, "acme", "bob", T0).unwrap();
    let s = use_token(&sql, &first, T0);
    let second = tokens::rotate(&sql, &s, T0).unwrap();
    let third = tokens::add(&sql, &s, "label", T0).unwrap();
    let (_, code) = tokens::reset(&sql, "acme", "sarah", "lachlan", T0).unwrap();

    let mut stored = String::new();
    for table in ["tokens", "signins"] {
        for row in sql.query(&format!("SELECT * FROM {table}"), &[]).unwrap() {
            stored.push_str(&serde_json::to_string(&row).unwrap());
        }
    }
    for secret in [&first, &second, &third, &code] {
        assert!(
            !stored.contains(secret.as_str()),
            "a secret is stored in the clear"
        );
        assert!(!stored.contains(secret.trim_start_matches("bch_")));
    }
    // What is shown of a token can't be turned back into it.
    assert_eq!(tokens::id_of(&first).len(), 8);
    assert!(!first.contains(&tokens::id_of(&first)));
}
