//! Bearer tokens for a relay's members: making them, checking them, replacing
//! them and taking them away.
//!
//! A token is a password a program holds. Only its SHA-256 is stored, so the
//! token itself exists in the one response that hands it over and wherever its
//! owner then keeps it. A member can hold several (one per machine, one for the
//! viewer), sees them by a short id, and can replace or revoke each on its own.
//!
//! Replacing a token is built so that it cannot lock anyone out:
//!
//! 1. `rotate` makes a successor and returns it. The old token is untouched.
//! 2. The first request made with the successor proves its holder has it. Only
//!    then does the old token get a short time left to live (`GRACE_MS`), which
//!    covers programs that read the old one a moment earlier.
//! 3. A successor nobody uses lapses (`UNUSED_SUCCESSOR_MS`) and the old token
//!    carries on: a reply lost on the way, or a crash before the new token was
//!    saved, leaves things as they were.
//!
//! Lookups by a token or a sign-in code are not scoped to an org: the
//! credential decides the org, as everywhere else on a relay. Everything else
//! here names the org and the member, and touches nobody else's rows.

use serde::Serialize;
use serde_json::json;

use crate::crypto::{b64, crockford, random_bytes, random_code, sha256_b64, unb64};
use crate::p;
use crate::sql::{add_column, int, opt_int, opt_text, text, tx, Row, Sql};
use crate::text::{clean_line, clip};
use crate::types::*;

/// How long a replaced token keeps working after its successor is first used.
pub const GRACE_MS: i64 = 60_000;
/// How long a successor waits for its first use before it lapses, leaving the
/// old token in place.
pub const UNUSED_SUCCESSOR_MS: i64 = 10 * 60_000;
/// How long an extra token (`add`) waits for its first use before it lapses.
pub const UNUSED_EXTRA_MS: i64 = 24 * 3_600_000;
/// The most tokens one member can hold at once, counting successors not yet
/// used and replaced tokens not yet dead. It also caps how fast tokens can be
/// rotated: about this many a minute.
pub const MAX_TOKENS: i64 = 10;
/// How long a sign-in code from an admin stays good.
pub const SIGN_IN_MS: i64 = 7 * 86_400_000;

/// The table as 0.1.0 made it, and the sign-in codes added since. `migrate`
/// brings an existing `tokens` table up to date by adding `ADDED` to it.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tokens (
  hash TEXT PRIMARY KEY,
  org TEXT NOT NULL,
  name TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_used INTEGER
);
CREATE TABLE IF NOT EXISTS signins (
  hash TEXT PRIMARY KEY,
  org TEXT NOT NULL,
  name TEXT NOT NULL,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
)";

/// Columns `tokens` gained after 0.1.0. Each has a default that reads, for a
/// row written before it existed, as "an ordinary token that never expires".
const ADDED: &[(&str, &str)] = &[
    // The program that last used the token, as it named itself.
    ("client", "TEXT NOT NULL DEFAULT ''"),
    // The owner's own label for it ("office laptop").
    ("note", "TEXT NOT NULL DEFAULT ''"),
    // When it stops working. NULL: never.
    ("expires_at", "INTEGER"),
    // The hash of the token this one retires on its first use.
    ("replaces", "TEXT"),
    // 1 until the token is first used.
    ("fresh", "INTEGER NOT NULL DEFAULT 0"),
];

/// Creates the tables, or adds what an older database is missing. Safe to run
/// every time a relay starts: it never rewrites or drops a row.
pub fn migrate<S: Sql>(sql: &S) -> Result<()> {
    sql.batch(SCHEMA)?;
    for (column, decl) in ADDED {
        add_column(sql, "tokens", column, decl)?;
    }
    Ok(())
}

/// A new token: `bch_` and 192 random bits.
fn new_secret() -> String {
    format!("bch_{}", b64(&random_bytes::<24>()))
}

/// What is stored in place of a token or a sign-in code.
pub fn hash(secret: &str) -> String {
    sha256_b64(secret.trim().as_bytes())
}

/// A token's short public name, e.g. "7KQ2M9XD": the first 40 bits of its
/// hash. Safe to show, and anyone holding the token can work it out.
pub fn id_of_hash(hash: &str) -> String {
    let bytes = unb64(hash).unwrap_or_default();
    crockford(&bytes[..bytes.len().min(5)])
}

pub fn id_of(token: &str) -> String {
    id_of_hash(&hash(token))
}

/// A sign-in code like "SIGN-7KQ2-M9XD-4TRW-B1ZP": 80 random bits.
fn sign_in_code() -> String {
    let c = random_code(16);
    format!(
        "SIGN-{}-{}-{}-{}",
        &c[0..4],
        &c[4..8],
        &c[8..12],
        &c[12..16]
    )
}

/// Who a token belongs to, and which token it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub org: String,
    pub name: String,
    pub hash: String,
}

impl Session {
    pub fn id(&self) -> String {
        id_of_hash(&self.hash)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenState {
    /// An ordinary token.
    Live,
    /// Made by `rotate` or `add` and not used yet. It lapses if it never is.
    New,
    /// Its successor is in use. It works until `expires_at`.
    Replaced,
}

#[derive(Clone, Debug, Serialize)]
pub struct TokenInfo {
    pub id: String,
    pub state: TokenState,
    /// The token the request asking was made with.
    pub current: bool,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub client: String,
    pub note: String,
}

fn info(r: &Row, current: Option<&str>) -> TokenInfo {
    let hash = text(r, "hash");
    let expires = opt_int(r, "expires_at");
    TokenInfo {
        id: id_of_hash(&hash),
        state: match (int(r, "fresh") != 0, expires.is_some()) {
            (true, _) => TokenState::New,
            (false, true) => TokenState::Replaced,
            (false, false) => TokenState::Live,
        },
        current: current == Some(hash.as_str()),
        created_at: crate::crypto::iso(int(r, "created_at")),
        last_used: opt_int(r, "last_used").map(crate::crypto::iso),
        expires_at: expires.map(crate::crypto::iso),
        client: text(r, "client"),
        note: text(r, "note"),
    }
}

/// Drops a member's tokens whose time has run out.
fn purge<S: Sql>(sql: &S, org: &str, name: &str, now: i64) -> Result<()> {
    sql.run(
        "DELETE FROM tokens WHERE org = ? AND name = ? AND expires_at IS NOT NULL AND expires_at <= ?",
        p![org, name, now],
    )
}

fn count<S: Sql>(sql: &S, org: &str, name: &str) -> Result<i64> {
    Ok(sql
        .one(
            "SELECT COUNT(*) AS n FROM tokens WHERE org = ? AND name = ?",
            p![org, name],
        )?
        .map(|r| int(&r, "n"))
        .unwrap_or(0))
}

fn too_many() -> Error {
    Error::new(
        Code::RateLimited,
        format!("you already hold {MAX_TOKENS} tokens, counting ones being replaced. Revoke one you don't need with `backchannel revoke-token <id>`, or wait a minute for replaced ones to lapse."),
    )
    .with(json!({ "retry_after": GRACE_MS / 1000 }))
}

/// An ordinary token for someone who has just joined or signed in again.
pub fn mint<S: Sql>(sql: &S, org: &str, name: &str, now: i64) -> Result<String> {
    let token = new_secret();
    sql.run(
        "INSERT INTO tokens (hash, org, name, created_at) VALUES (?, ?, ?, ?)",
        p![hash(&token), org, name, now],
    )?;
    Ok(token)
}

/// Checks a token and records its use. `None` when it is unknown, revoked or
/// past its time: those are indistinguishable, because nothing is kept of a
/// token that has gone.
///
/// The first use of a successor is what retires the token it replaces.
pub fn authenticate<S: Sql>(
    sql: &S,
    token: &str,
    client: &str,
    now: i64,
) -> Result<Option<Session>> {
    let token = token.trim();
    if token.is_empty() {
        return Ok(None);
    }
    let hash = hash(token);
    tx(sql, || {
        let Some(r) = sql.one("SELECT * FROM tokens WHERE hash = ?", p![&hash])? else {
            return Ok(None);
        };
        let (org, name) = (text(&r, "org"), text(&r, "name"));
        if opt_int(&r, "expires_at").is_some_and(|at| at <= now) {
            purge(sql, &org, &name, now)?;
            return Ok(None);
        }
        if int(&r, "fresh") != 0 {
            // Its holder has it and it works: from here on this is the real one.
            if let Some(old) = opt_text(&r, "replaces") {
                sql.run(
                    "UPDATE tokens SET expires_at = ? WHERE hash = ? AND org = ? AND name = ? AND fresh = 0 AND expires_at IS NULL",
                    p![now + GRACE_MS, old, &org, &name],
                )?;
            }
            sql.run(
                "UPDATE tokens SET fresh = 0, expires_at = NULL, replaces = NULL WHERE hash = ?",
                p![&hash],
            )?;
        }
        sql.run(
            "UPDATE tokens SET last_used = ?, client = ? WHERE hash = ?",
            p![now, clip(&clean_line(client), limits::CLIENT), &hash],
        )?;
        Ok(Some(Session { org, name, hash }))
    })
}

/// Makes a successor to the token in `s` and returns it. Nothing else changes
/// until the successor is used: see the module notes.
pub fn rotate<S: Sql>(sql: &S, s: &Session, now: i64) -> Result<String> {
    tx(sql, || {
        purge(sql, &s.org, &s.name, now)?;
        let Some(r) = sql.one(
            "SELECT note, expires_at FROM tokens WHERE hash = ? AND org = ? AND name = ?",
            p![&s.hash, &s.org, &s.name],
        )?
        else {
            return err(Code::Unauthorized, "that token no longer works");
        };
        if opt_int(&r, "expires_at").is_some() {
            return err(Code::Conflict, "this token has already been replaced, and stops working shortly. Rotate with the new one.");
        }
        // Asking again replaces the successor made last time, used or not.
        sql.run(
            "DELETE FROM tokens WHERE org = ? AND name = ? AND replaces = ? AND fresh = 1",
            p![&s.org, &s.name, &s.hash],
        )?;
        if count(sql, &s.org, &s.name)? >= MAX_TOKENS {
            return Err(too_many());
        }
        let token = new_secret();
        sql.run(
            "INSERT INTO tokens (hash, org, name, created_at, note, expires_at, replaces, fresh) VALUES (?, ?, ?, ?, ?, ?, ?, 1)",
            p![hash(&token), &s.org, &s.name, now, text(&r, "note"), now + UNUSED_SUCCESSOR_MS, &s.hash],
        )?;
        Ok(token)
    })
}

/// Makes one more token for the member in `s`, for another machine or the
/// viewer. It lapses if it is not used within `UNUSED_EXTRA_MS`.
pub fn add<S: Sql>(sql: &S, s: &Session, note: &str, now: i64) -> Result<String> {
    tx(sql, || {
        purge(sql, &s.org, &s.name, now)?;
        if count(sql, &s.org, &s.name)? >= MAX_TOKENS {
            return Err(too_many());
        }
        let token = new_secret();
        sql.run(
            "INSERT INTO tokens (hash, org, name, created_at, note, expires_at, fresh) VALUES (?, ?, ?, ?, ?, ?, 1)",
            p![hash(&token), &s.org, &s.name, now, clip(&clean_line(note), limits::NOTE), now + UNUSED_EXTRA_MS],
        )?;
        Ok(token)
    })
}

/// A member's tokens, oldest first. `current` is the hash of the token asking.
pub fn list<S: Sql>(
    sql: &S,
    org: &str,
    name: &str,
    current: Option<&str>,
    now: i64,
) -> Result<Vec<TokenInfo>> {
    purge(sql, org, name, now)?;
    Ok(sql
        .query(
            "SELECT * FROM tokens WHERE org = ? AND name = ? ORDER BY created_at, hash",
            p![org, name],
        )?
        .iter()
        .map(|r| info(r, current))
        .collect())
}

/// Revokes one of the member's own tokens, by its id. Not the one asking:
/// that is what `rotate` is for, and revoking it would leave its holder with
/// nothing.
pub fn revoke<S: Sql>(sql: &S, s: &Session, id: &str, now: i64) -> Result<TokenInfo> {
    let id = id.trim().to_uppercase();
    tx(sql, || {
        purge(sql, &s.org, &s.name, now)?;
        let rows = sql.query(
            "SELECT * FROM tokens WHERE org = ? AND name = ?",
            p![&s.org, &s.name],
        )?;
        let Some(r) = rows.iter().find(|r| id_of_hash(&text(r, "hash")) == id) else {
            return err(
                Code::NotFound,
                format!("you have no token with the id {id}; list them with `backchannel tokens`"),
            );
        };
        let hash = text(r, "hash");
        if hash == s.hash {
            return err(Code::Invalid, "that is the token this command is using. To replace it, run `backchannel rotate`. To kill it, revoke it from another machine or ask your org's admin.");
        }
        // A successor waiting to replace it has nothing left to replace.
        sql.run(
            "DELETE FROM tokens WHERE org = ? AND name = ? AND (hash = ? OR (replaces = ? AND fresh = 1 AND hash != ?))",
            p![&s.org, &s.name, &hash, &hash, &s.hash],
        )?;
        Ok(info(r, Some(&s.hash)))
    })
}

/// Takes away every token a member holds, and any sign-in code waiting for
/// them. Returns how many tokens went.
pub fn revoke_all<S: Sql>(sql: &S, org: &str, name: &str) -> Result<i64> {
    tx(sql, || {
        let n = count(sql, org, name)?;
        sql.run(
            "DELETE FROM tokens WHERE org = ? AND name = ?",
            p![org, name],
        )?;
        sql.run(
            "DELETE FROM signins WHERE org = ? AND name = ?",
            p![org, name],
        )?;
        Ok(n)
    })
}

/// Signs a member out everywhere and returns how many tokens went, with a code
/// they can sign back in with once. A member has at most one code waiting.
pub fn reset<S: Sql>(sql: &S, org: &str, name: &str, by: &str, now: i64) -> Result<(i64, String)> {
    tx(sql, || {
        let n = revoke_all(sql, org, name)?;
        let code = sign_in_code();
        sql.run(
            "INSERT INTO signins (hash, org, name, created_by, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
            p![hash(&code), org, name, by, now, now + SIGN_IN_MS],
        )?;
        Ok((n, code))
    })
}

/// Uses up a sign-in code and says whose it was. `None` when it is wrong, used
/// or too old. The caller checks the member is still one, then calls `mint`.
pub fn redeem<S: Sql>(sql: &S, code: &str, now: i64) -> Result<Option<(String, String)>> {
    let hash = hash(&code.trim().to_uppercase());
    tx(sql, || {
        let Some(r) = sql.one(
            "SELECT org, name, expires_at FROM signins WHERE hash = ?",
            p![&hash],
        )?
        else {
            return Ok(None);
        };
        let (org, name) = (text(&r, "org"), text(&r, "name"));
        sql.run(
            "DELETE FROM signins WHERE hash = ? AND org = ? AND name = ?",
            p![&hash, &org, &name],
        )?;
        Ok((int(&r, "expires_at") > now).then_some((org, name)))
    })
}

/// For an admin: each member's token count and when one was last used.
pub fn summary<S: Sql>(sql: &S, org: &str, now: i64) -> Result<Vec<Row>> {
    sql.query(
        "SELECT name, COUNT(*) AS tokens, MAX(last_used) AS last_used FROM tokens WHERE org = ? AND (expires_at IS NULL OR expires_at > ?) GROUP BY name",
        p![org, now],
    )
}
