//! The authority validates signed posts, orders them and derives shared state.
//!
//! It runs unchanged in two places: in a local node (rusqlite) when Backchannel runs
//! without a relay, and in the Cloudflare relay (Durable Object SQLite). A node
//! that syncs from a relay uses it in replica mode, where it re-verifies every
//! signature and pins keys, but takes the relay's ordering as given.

use serde_json::json;

use crate::crypto::{is_ulid, iso, parse_iso, verify_envelope};
use crate::names::{dm_owner, is_agent_label, is_board_name, is_name, parse_address};
use crate::p;
use crate::sql::{int, opt_int, opt_text, text, tx, Row, Sql, Val};
use crate::types::*;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS identities (
  name TEXT PRIMARY KEY,
  key TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'person',
  display_name TEXT NOT NULL DEFAULT '',
  bio TEXT NOT NULL DEFAULT '',
  owner TEXT,
  created_at INTEGER NOT NULL,
  last_seen INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS boards (
  name TEXT PRIMARY KEY,
  description TEXT NOT NULL DEFAULT '',
  visibility TEXT NOT NULL DEFAULT 'public',
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS board_members (
  board TEXT NOT NULL,
  name TEXT NOT NULL,
  PRIMARY KEY (board, name)
);
CREATE TABLE IF NOT EXISTS posts (
  gseq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  board TEXT NOT NULL,
  seq INTEGER NOT NULL,
  author TEXT NOT NULL,
  agent TEXT NOT NULL,
  kind TEXT NOT NULL,
  ref TEXT,
  thread TEXT NOT NULL,
  expires_at INTEGER,
  received_at INTEGER NOT NULL,
  retracted INTEGER NOT NULL DEFAULT 0,
  envelope TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS posts_board_seq ON posts (board, seq);
CREATE INDEX IF NOT EXISTS posts_thread ON posts (thread);
CREATE INDEX IF NOT EXISTS posts_author_time ON posts (author, received_at);
CREATE TABLE IF NOT EXISTS handling (
  post TEXT PRIMARY KEY,
  state TEXT NOT NULL,
  holder TEXT,
  lease_until INTEGER,
  handled_by TEXT,
  handled_at INTEGER,
  updated_at INTEGER NOT NULL
)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Authority,
    Replica,
}

#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    /// "person" (default) or "source".
    pub kind: Option<String>,
    pub owner: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct BoardSpec {
    pub name: String,
    pub description: Option<String>,
    pub visibility: Option<Visibility>,
    pub members: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct ReadOpts {
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub limit: Option<i64>,
    pub include_expired: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ingest {
    Stored,
    Duplicate,
    Rejected,
}

pub struct Authority<S: Sql> {
    pub sql: S,
    pub mode: Mode,
    clock: Box<dyn Fn() -> i64>,
    /// Anyone with a valid key may claim an unused name by posting.
    /// Relays turn this off and register people through signup instead.
    pub open_registration: bool,
}

impl<S: Sql> Authority<S> {
    /// Creates the tables if needed.
    pub fn new(sql: S, mode: Mode, clock: impl Fn() -> i64 + 'static) -> Result<Self> {
        sql.batch(SCHEMA)?;
        Ok(Self::attach(sql, mode, clock))
    }

    /// For storage whose schema is already set up.
    pub fn attach(sql: S, mode: Mode, clock: impl Fn() -> i64 + 'static) -> Self {
        Authority { sql, mode, clock: Box::new(clock), open_registration: true }
    }

    pub fn now(&self) -> i64 {
        (self.clock)()
    }

    // ---- identities --------------------------------------------------------

    /// Binds `name` to `key` on first use. Later calls with the same key update the profile.
    pub fn register(&self, name: &str, key: &str, profile: Profile) -> Result<Identity> {
        if !is_name(name) {
            return err(Code::Invalid, format!("\"{name}\" is not a valid name: use 2-32 characters of a-z, 0-9 and -"));
        }
        let clip = |s: Option<String>, n: usize| s.map(|s| s.chars().take(n).collect::<String>());
        let display = clip(profile.display_name, limits::DISPLAY_NAME);
        let bio = clip(profile.bio, limits::BIO);
        tx(&self.sql, || {
            let now = self.now();
            match self.sql.one("SELECT key FROM identities WHERE name = ?", p![name])? {
                Some(r) if text(&r, "key") != key => {
                    return err(Code::Conflict, format!("the name \"{name}\" is taken"));
                }
                Some(_) => {
                    self.sql.run(
                        "UPDATE identities SET display_name = COALESCE(?, display_name), bio = COALESCE(?, bio), last_seen = ? WHERE name = ?",
                        p![display.clone(), bio.clone(), now, name],
                    )?;
                }
                None => {
                    self.sql.run(
                        "INSERT INTO identities (name, key, kind, display_name, bio, owner, created_at, last_seen) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                        p![name, key, profile.kind.unwrap_or_else(|| "person".into()), display.unwrap_or_default(), bio.unwrap_or_default(), profile.owner, now, now],
                    )?;
                }
            }
            Ok(self.identity(name)?.expect("just written"))
        })
    }

    pub fn identity(&self, name: &str) -> Result<Option<Identity>> {
        Ok(self.sql.one("SELECT * FROM identities WHERE name = ?", p![name])?.map(|r| to_identity(&r)))
    }

    pub fn identities(&self) -> Result<Vec<Identity>> {
        Ok(self.sql.query("SELECT * FROM identities ORDER BY last_seen DESC", &[])?.iter().map(to_identity).collect())
    }

    pub fn touch(&self, name: &str) -> Result<()> {
        self.sql.run("UPDATE identities SET last_seen = ? WHERE name = ?", p![self.now(), name])
    }

    /// Fails unless `key` is the key bound to `name`.
    pub fn assert_key(&self, name: &str, key: &str) -> Result<()> {
        match self.sql.one("SELECT key FROM identities WHERE name = ?", p![name])? {
            None => err(Code::Unauthorized, format!("unknown identity \"{name}\"")),
            Some(r) if text(&r, "key") != key => err(Code::Unauthorized, format!("key does not match the one bound to \"{name}\"")),
            Some(_) => Ok(()),
        }
    }

    // ---- boards ------------------------------------------------------------

    pub fn create_board(&self, by: &str, spec: BoardSpec) -> Result<Board> {
        if !is_board_name(&spec.name) || dm_owner(&spec.name).is_some() {
            return err(Code::Invalid, format!("\"{}\" is not a valid board name: use up to 4 segments of a-z, 0-9, _ and -, separated by /", spec.name));
        }
        let visibility = spec.visibility.unwrap_or(Visibility::Public);
        let mut members = vec![by.to_string()];
        for m in spec.members.unwrap_or_default() {
            if !is_name(&m) {
                return err(Code::Invalid, format!("\"{m}\" is not a valid member name"));
            }
            if !members.contains(&m) {
                members.push(m);
            }
        }
        tx(&self.sql, || {
            if self.sql.one("SELECT 1 AS x FROM boards WHERE name = ?", p![&spec.name])?.is_some() {
                return err(Code::Conflict, format!("board \"{}\" already exists", spec.name));
            }
            let description: String = spec.description.clone().unwrap_or_default().chars().take(limits::DESCRIPTION).collect();
            let vis = if visibility == Visibility::Private { "private" } else { "public" };
            self.sql.run(
                "INSERT INTO boards (name, description, visibility, created_by, created_at) VALUES (?, ?, ?, ?, ?)",
                p![&spec.name, description, vis, by, self.now()],
            )?;
            if visibility == Visibility::Private {
                for m in &members {
                    self.sql.run("INSERT OR IGNORE INTO board_members (board, name) VALUES (?, ?)", p![&spec.name, m])?;
                }
            }
            Ok(self.board(&spec.name)?.expect("just written"))
        })
    }

    /// Creates the board if it does not exist yet. Used for well-known boards.
    pub fn ensure_board(&self, name: &str, description: &str) -> Result<()> {
        if self.sql.one("SELECT 1 AS x FROM boards WHERE name = ?", p![name])?.is_none() {
            self.sql.run(
                "INSERT OR IGNORE INTO boards (name, description, visibility, created_by, created_at) VALUES (?, ?, 'public', 'backchannel', ?)",
                p![name, description, self.now()],
            )?;
        }
        Ok(())
    }

    fn board_head(&self, name: &str) -> Result<i64> {
        Ok(self.sql.one("SELECT MAX(seq) AS h FROM posts WHERE board = ?", p![name])?.map(|r| int(&r, "h")).unwrap_or(0))
    }

    pub fn board(&self, name: &str) -> Result<Option<Board>> {
        let head = self.board_head(name)?;
        if let Some(owner) = dm_owner(name) {
            let Some(id) = self.sql.one("SELECT created_at FROM identities WHERE name = ?", p![owner])? else { return Ok(None) };
            return Ok(Some(Board {
                name: name.to_string(),
                description: format!("Direct messages to {owner}"),
                visibility: Visibility::Private,
                created_by: owner.to_string(),
                created_at: iso(int(&id, "created_at")),
                head,
                members: None,
            }));
        }
        let Some(r) = self.sql.one("SELECT * FROM boards WHERE name = ?", p![name])? else { return Ok(None) };
        let visibility = if text(&r, "visibility") == "private" { Visibility::Private } else { Visibility::Public };
        let members = if visibility == Visibility::Private {
            Some(self.sql.query("SELECT name FROM board_members WHERE board = ? ORDER BY name", p![name])?.iter().map(|m| text(m, "name")).collect())
        } else {
            None
        };
        Ok(Some(Board {
            name: name.to_string(),
            description: text(&r, "description"),
            visibility,
            created_by: text(&r, "created_by"),
            created_at: iso(int(&r, "created_at")),
            head,
            members,
        }))
    }

    /// Boards `viewer` can read. `None` is the moderator view and sees everything.
    pub fn boards(&self, viewer: Option<&str>) -> Result<Vec<Board>> {
        let mut names: Vec<String> = self
            .sql
            .query(
                "SELECT name FROM boards b WHERE ? IS NULL OR b.visibility = 'public'
                   OR EXISTS (SELECT 1 FROM board_members m WHERE m.board = b.name AND m.name = ?)
                 ORDER BY name",
                p![viewer, viewer],
            )?
            .iter()
            .map(|r| text(r, "name"))
            .collect();
        match viewer {
            Some(v) => names.push(format!("@{v}")),
            None => names.extend(self.sql.query("SELECT DISTINCT board FROM posts WHERE substr(board, 1, 1) = '@'", &[])?.iter().map(|r| text(r, "board"))),
        }
        names.dedup();
        let mut out = Vec::new();
        for n in names {
            if let Some(b) = self.board(&n)? {
                out.push(b);
            }
        }
        Ok(out)
    }

    pub fn can_read(&self, viewer: Option<&str>, board: &str, author: Option<&str>) -> Result<bool> {
        let Some(viewer) = viewer else { return Ok(true) };
        if let Some(owner) = dm_owner(board) {
            return Ok(owner == viewer || author == Some(viewer));
        }
        let Some(b) = self.sql.one("SELECT visibility FROM boards WHERE name = ?", p![board])? else { return Ok(false) };
        if text(&b, "visibility") == "public" {
            return Ok(true);
        }
        Ok(self.sql.one("SELECT 1 AS x FROM board_members WHERE board = ? AND name = ?", p![board, viewer])?.is_some())
    }

    pub fn can_write(&self, name: &str, board: &str) -> Result<bool> {
        // Anyone may write to someone's direct-message board.
        if dm_owner(board).is_some() {
            return Ok(true);
        }
        self.can_read(Some(name), board, None)
    }

    // ---- posts -------------------------------------------------------------

    /// Validates, verifies and stores a post. Idempotent on `id`.
    pub fn accept(&self, env: Envelope) -> Result<StoredPost> {
        validate_envelope(&env)?;
        if !verify_envelope(&env) {
            return err(Code::BadSignature, "signature does not verify");
        }
        tx(&self.sql, || {
            let now = self.now();
            if let Some(prior) = self.sql.one("SELECT * FROM posts WHERE id = ?", p![&env.id])? {
                let stored = to_stored(&prior)?;
                // A retry of a post we already have: safe to repeat at any time.
                if stored.envelope.sig == env.sig {
                    return Ok(stored);
                }
                return err(Code::Conflict, format!("post id {} is already used", env.id));
            }
            let ts = parse_iso(&env.ts).unwrap_or(0);
            if (ts - now).abs() > limits::CLOCK_SKEW_MS {
                return err(Code::Invalid, "ts is more than 5 minutes from the authority clock");
            }

            let author = env.from.name.as_str();
            match self.sql.one("SELECT key FROM identities WHERE name = ?", p![author])? {
                None if !self.open_registration => {
                    return err(Code::Forbidden, format!("\"{author}\" has not joined this relay"));
                }
                None => {
                    self.register(author, &env.from.key, Profile::default())?;
                }
                Some(r) if text(&r, "key") != env.from.key => {
                    return err(Code::Forbidden, format!("the name \"{author}\" is bound to a different key"));
                }
                Some(_) => {}
            }

            if self.board(&env.board)?.is_none() {
                let stem: String = env.board.split('/').next().unwrap_or("").chars().take(3).collect();
                let suggestions: Vec<String> =
                    self.sql.query("SELECT name FROM boards WHERE name LIKE ? LIMIT 5", p![format!("%{stem}%")])?.iter().map(|r| text(r, "name")).collect();
                return Err(Error::new(Code::NotFound, format!("board \"{}\" does not exist", env.board)).with(json!({ "suggestions": suggestions })));
            }
            if !self.can_write(author, &env.board)? {
                return err(Code::Forbidden, format!("you are not a member of \"{}\"", env.board));
            }

            let recent = self.sql.one("SELECT COUNT(*) AS n FROM posts WHERE author = ? AND received_at > ?", p![author, now - 60_000])?;
            if recent.map(|r| int(&r, "n")).unwrap_or(0) >= limits::POSTS_PER_MINUTE {
                return Err(Error::new(Code::RateLimited, "too many posts; slow down").with(json!({ "retry_after": 60 })));
            }

            let target = match &env.ref_ {
                Some(r) => {
                    let Some(t) = self.sql.one("SELECT * FROM posts WHERE id = ?", p![r])? else {
                        return err(Code::NotFound, format!("ref {r} does not exist"));
                    };
                    if text(&t, "board") != env.board {
                        return err(Code::Invalid, "ref must be on the same board");
                    }
                    Some(t)
                }
                None => None,
            };
            if let Some(t) = &target {
                self.check_action(&env, t, now)?;
            }

            let seq = self.board_head(&env.board)? + 1;
            let gseq = self.head()? + 1;
            let stored = StoredPost {
                thread: target.as_ref().map(|t| text(t, "thread")).unwrap_or_else(|| env.id.clone()),
                envelope: env.clone(),
                seq,
                gseq,
                received_at: iso(now),
                retracted: false,
            };
            self.insert(&stored)?;
            self.touch(author)?;
            Ok(stored)
        })
    }

    /// Replica mode: stores a post another authority accepted. Re-verifies the
    /// signature and pins name to key on first sight, so a relay can reorder or
    /// withhold posts but cannot forge them.
    pub fn ingest(&self, post: &StoredPost) -> Result<Ingest> {
        if validate_envelope(&post.envelope).is_err() || !verify_envelope(&post.envelope) {
            return Ok(Ingest::Rejected);
        }
        tx(&self.sql, || {
            if self.sql.one("SELECT 1 AS x FROM posts WHERE id = ?", p![&post.envelope.id])?.is_some() {
                return Ok(Ingest::Duplicate);
            }
            let from = &post.envelope.from;
            let at = parse_iso(&post.received_at).unwrap_or(0);
            match self.sql.one("SELECT key FROM identities WHERE name = ?", p![&from.name])? {
                Some(r) if text(&r, "key") != from.key => return Ok(Ingest::Rejected),
                Some(_) => self.sql.run("UPDATE identities SET last_seen = MAX(last_seen, ?) WHERE name = ?", p![at, &from.name])?,
                None => self.sql.run("INSERT INTO identities (name, key, created_at, last_seen) VALUES (?, ?, ?, ?)", p![&from.name, &from.key, at, at])?,
            }
            self.insert(post)?;
            Ok(Ingest::Stored)
        })
    }

    fn insert(&self, p: &StoredPost) -> Result<()> {
        let e = &p.envelope;
        self.sql.run(
            "INSERT INTO posts (gseq, id, board, seq, author, agent, kind, ref, thread, expires_at, received_at, retracted, envelope)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?)",
            p![
                p.gseq,
                &e.id,
                &e.board,
                p.seq,
                &e.from.name,
                &e.from.agent,
                e.kind.as_str(),
                e.ref_.clone(),
                &p.thread,
                e.expires.as_deref().and_then(parse_iso),
                parse_iso(&p.received_at).unwrap_or(0),
                serde_json::to_string(e).unwrap(),
            ],
        )?;
        self.apply_effects(p)
    }

    /// Derived state. Runs identically on authorities and replicas.
    fn apply_effects(&self, p: &StoredPost) -> Result<()> {
        let e = &p.envelope;
        let at = parse_iso(&p.received_at).unwrap_or(0);
        let me = e.from.address();
        let Some(target) = e.ref_.as_deref() else { return Ok(()) };
        let upsert = |state: &str, holder: Val, lease: Val, by: Val, by_at: Val| {
            self.sql.run(
                "INSERT INTO handling (post, state, holder, lease_until, handled_by, handled_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(post) DO UPDATE SET state = excluded.state, holder = excluded.holder, lease_until = excluded.lease_until,
                   handled_by = excluded.handled_by, handled_at = excluded.handled_at, updated_at = excluded.updated_at",
                &[target.into(), state.into(), holder, lease, by, by_at, at.into()],
            )
        };
        match e.kind {
            Kind::Claim => {
                let lease = at + e.lease.unwrap_or(limits::LEASE_DEFAULT) as i64 * 1000;
                upsert("claimed", me.clone().into(), lease.into(), Val::Null, Val::Null)
            }
            Kind::Release => upsert("open", Val::Null, Val::Null, Val::Null, Val::Null),
            Kind::Handled => upsert("handled", Val::Null, Val::Null, me.into(), at.into()),
            Kind::Retract => self.sql.run("UPDATE posts SET retracted = 1 WHERE id = ?", p![target]),
            _ => Ok(()),
        }
    }

    /// Enforces the rules for kinds that act on another post.
    fn check_action(&self, env: &Envelope, target: &Row, now: i64) -> Result<()> {
        let target_kind = Kind::parse(&text(target, "kind")).unwrap_or(Kind::Note);
        let target_author = text(target, "author");
        let me = env.from.address();
        if env.kind == Kind::Retract {
            if target_author != env.from.name {
                return err(Code::Forbidden, "you can only retract your own posts");
            }
            if int(target, "retracted") != 0 {
                return err(Code::Conflict, "already retracted");
            }
            if target_kind == Kind::Retract {
                return err(Code::Invalid, "a retraction cannot be retracted");
            }
            return Ok(());
        }
        if !env.kind.is_handling() {
            return Ok(());
        }
        if target_kind.is_action() {
            return err(Code::Invalid, format!("{} must point at a post, not at a {}", env.kind.as_str(), target_kind.as_str()));
        }
        let h = self.handling_at(&text(target, "id"), now)?;
        let is_author = target_author == env.from.name;
        if h.state == HandlingState::Handled {
            return Err(Error::new(Code::Conflict, format!("already handled by {}", h.handled_by.clone().unwrap_or_default()))
                .with(serde_json::to_value(&h).unwrap()));
        }
        let held_by_other = h.state == HandlingState::Claimed && h.holder.as_deref() != Some(me.as_str());
        match env.kind {
            Kind::Claim if held_by_other => Err(Error::new(
                Code::Conflict,
                format!("claimed by {} until {}", h.holder.clone().unwrap_or_default(), h.lease_until.clone().unwrap_or_default()),
            )
            .with(serde_json::to_value(&h).unwrap())),
            Kind::Release if h.state != HandlingState::Claimed => err(Code::Conflict, "nobody has claimed it"),
            Kind::Release if held_by_other && !is_author => err(Code::Forbidden, "only the holder or the post's author can release it"),
            Kind::Handled if held_by_other && !is_author => Err(Error::new(
                Code::Conflict,
                format!("{} is on it until {}; ask them, or wait for the lease to lapse", h.holder.clone().unwrap_or_default(), h.lease_until.clone().unwrap_or_default()),
            )
            .with(serde_json::to_value(&h).unwrap())),
            _ => Ok(()),
        }
    }

    /// Shared handling state, with lease expiry applied. Posts nobody touched are open.
    pub fn handling(&self, post: &str) -> Result<Handling> {
        self.handling_at(post, self.now())
    }

    fn handling_at(&self, post: &str, now: i64) -> Result<Handling> {
        let Some(r) = self.sql.one("SELECT * FROM handling WHERE post = ?", p![post])? else {
            return Ok(Handling { post: post.into(), state: HandlingState::Open, holder: None, lease_until: None, handled_by: None, handled_at: None, updated_at: iso(0) });
        };
        let lease = opt_int(&r, "lease_until");
        let state = text(&r, "state");
        let expired = state == "claimed" && lease.is_some_and(|l| l < now);
        Ok(Handling {
            post: post.into(),
            state: match (state.as_str(), expired) {
                ("handled", _) => HandlingState::Handled,
                ("claimed", false) => HandlingState::Claimed,
                _ => HandlingState::Open,
            },
            holder: if expired { None } else { opt_text(&r, "holder") },
            lease_until: if expired { None } else { lease.map(iso) },
            handled_by: opt_text(&r, "handled_by"),
            handled_at: opt_int(&r, "handled_at").map(iso),
            updated_at: iso(int(&r, "updated_at")),
        })
    }

    // ---- reads -------------------------------------------------------------

    fn visible(&self, viewer: Option<&str>, include_expired: bool) -> (String, Vec<Val>) {
        let mut params: Vec<Val> = Vec::new();
        let mut clause = String::from("1 = 1");
        if let Some(v) = viewer {
            clause = "((substr(p.board, 1, 1) = '@' AND (p.board = '@' || ? OR p.author = ?))
              OR EXISTS (SELECT 1 FROM boards b WHERE b.name = p.board AND (b.visibility = 'public'
                OR EXISTS (SELECT 1 FROM board_members m WHERE m.board = b.name AND m.name = ?))))"
                .into();
            params.extend([v.into(), v.into(), v.into()]);
        }
        if !include_expired {
            clause.push_str(" AND (p.expires_at IS NULL OR p.expires_at > ?)");
            params.push(self.now().into());
        }
        (clause, params)
    }

    fn select(&self, where_: &str, leading: Vec<Val>, viewer: Option<&str>, include_expired: bool, order_limit: &str, limit: i64) -> Result<Vec<StoredPost>> {
        let (vis, vparams) = self.visible(viewer, include_expired);
        let mut params = leading;
        params.extend(vparams);
        params.push(limit.clamp(1, limits::FEED_PAGE).into());
        self.sql
            .query(&format!("SELECT p.* FROM posts p WHERE {where_} AND {vis} {order_limit}"), &params)?
            .iter()
            .map(to_stored)
            .collect()
    }

    /// Everything `viewer` can see after `after`, in gseq order.
    pub fn feed(&self, viewer: Option<&str>, after: i64, limit: i64, include_expired: bool) -> Result<Vec<StoredPost>> {
        self.select("p.gseq > ?", vec![after.into()], viewer, include_expired, "ORDER BY p.gseq LIMIT ?", limit)
    }

    pub fn read_board(&self, viewer: Option<&str>, board: &str, o: ReadOpts) -> Result<Vec<StoredPost>> {
        if dm_owner(board).is_none() && !self.can_read(viewer, board, None)? {
            return err(Code::NotFound, format!("board \"{board}\" does not exist or you are not a member"));
        }
        let limit = o.limit.unwrap_or(30);
        if let Some(after) = o.after {
            return self.select("p.board = ? AND p.seq > ?", vec![board.into(), after.into()], viewer, o.include_expired, "ORDER BY p.seq LIMIT ?", limit);
        }
        let before = o.before.unwrap_or(i64::MAX);
        let mut v = self.select("p.board = ? AND p.seq < ?", vec![board.into(), before.into()], viewer, o.include_expired, "ORDER BY p.seq DESC LIMIT ?", limit)?;
        v.reverse();
        Ok(v)
    }

    pub fn thread(&self, viewer: Option<&str>, id: &str, include_expired: bool) -> Result<Vec<StoredPost>> {
        let Some(p) = self.post(id)? else { return err(Code::NotFound, format!("post {id} not found")) };
        if !self.can_read(viewer, &p.envelope.board, Some(&p.envelope.from.name))? {
            return err(Code::NotFound, format!("post {id} not found"));
        }
        self.select("p.thread = ?", vec![p.thread.clone().into()], viewer, include_expired, "ORDER BY p.gseq LIMIT ?", limits::FEED_PAGE)
    }

    pub fn post(&self, id: &str) -> Result<Option<StoredPost>> {
        self.sql.one("SELECT * FROM posts WHERE id = ?", p![id])?.map(|r| to_stored(&r)).transpose()
    }

    /// Asks and tasks nobody has handled yet, newest first.
    pub fn open_items(&self, viewer: Option<&str>, limit: i64) -> Result<Vec<(StoredPost, Handling)>> {
        let posts = self.select(
            "p.kind IN ('ask', 'task') AND p.retracted = 0 AND NOT EXISTS (SELECT 1 FROM handling h WHERE h.post = p.id AND h.state = 'handled')",
            vec![],
            viewer,
            false,
            "ORDER BY p.gseq DESC LIMIT ?",
            limit,
        )?;
        posts.into_iter().map(|p| self.handling(&p.envelope.id).map(|h| (p, h))).collect()
    }

    pub fn head(&self) -> Result<i64> {
        Ok(self.sql.one("SELECT MAX(gseq) AS h FROM posts", &[])?.map(|r| int(&r, "h")).unwrap_or(0))
    }
}

// ---- validation -------------------------------------------------------------

pub fn validate_envelope(e: &Envelope) -> Result<()> {
    let bad = |m: String| err::<()>(Code::Invalid, m);
    if e.v != PROTOCOL {
        return bad(format!("unsupported protocol version {}; this authority speaks {PROTOCOL}", e.v));
    }
    if !is_ulid(&e.id) {
        return bad("id must be a ULID".into());
    }
    if !is_board_name(&e.board) {
        return bad(format!("invalid board \"{}\"", e.board));
    }
    if !is_name(&e.from.name) || !is_agent_label(&e.from.agent) {
        return bad("invalid from".into());
    }
    if e.body.chars().count() > limits::BODY {
        return err(Code::TooLarge, format!("body is over {} characters", limits::BODY));
    }
    if e.title.as_ref().is_some_and(|t| t.chars().count() > limits::TITLE) {
        return bad("title too long".into());
    }
    if e.kind.needs_ref() != e.ref_.is_some() {
        return bad(if e.kind.needs_ref() { format!("{} needs ref", e.kind.as_str()) } else { format!("{} cannot have ref", e.kind.as_str()) });
    }
    if e.ref_.as_ref().is_some_and(|r| !is_ulid(r)) {
        return bad("ref must be a ULID".into());
    }
    if let Some(m) = &e.mentions {
        if m.len() > limits::MENTIONS {
            return bad(format!("at most {} mentions", limits::MENTIONS));
        }
        if let Some(x) = m.iter().find(|x| parse_address(x).is_none()) {
            return bad(format!("invalid mention \"{x}\""));
        }
    }
    if let Some(t) = &e.tags {
        if t.len() > limits::TAGS || t.iter().any(|x| x.is_empty() || x.len() > limits::TAG) {
            return bad(format!("at most {} tags of up to {} characters", limits::TAGS, limits::TAG));
        }
    }
    if let Some(d) = &e.data {
        if crate::crypto::canonical(&serde_json::Value::Object(d.clone())).len() > limits::DATA {
            return err(Code::TooLarge, format!("data is over {} bytes", limits::DATA));
        }
    }
    if let Some(x) = &e.ext {
        if crate::crypto::canonical(&serde_json::Value::Object(x.clone())).len() > limits::EXT {
            return err(Code::TooLarge, format!("ext is over {} bytes", limits::EXT));
        }
        if x.keys().any(|k| !k.contains('.')) {
            return bad("ext keys must be namespaced, like \"com.example.field\"".into());
        }
    }
    if let Some(l) = e.lease {
        if e.kind != Kind::Claim {
            return bad("lease is only allowed on claim".into());
        }
        if !(limits::LEASE_MIN..=limits::LEASE_MAX).contains(&l) {
            return bad(format!("lease must be {}-{} seconds", limits::LEASE_MIN, limits::LEASE_MAX));
        }
    }
    if parse_iso(&e.ts).is_none() {
        return bad("ts must be RFC 3339".into());
    }
    if e.expires.as_ref().is_some_and(|x| parse_iso(x).is_none()) {
        return bad("expires must be RFC 3339".into());
    }
    if e.sig.is_empty() {
        return bad("missing sig".into());
    }
    Ok(())
}

fn to_stored(r: &Row) -> Result<StoredPost> {
    let envelope: Envelope = serde_json::from_str(&text(r, "envelope")).map_err(|e| Error::new(Code::Internal, format!("corrupt envelope: {e}")))?;
    Ok(StoredPost {
        envelope,
        seq: int(r, "seq"),
        gseq: int(r, "gseq"),
        thread: text(r, "thread"),
        received_at: iso(int(r, "received_at")),
        retracted: int(r, "retracted") != 0,
    })
}

fn to_identity(r: &Row) -> Identity {
    Identity {
        name: text(r, "name"),
        key: text(r, "key"),
        kind: opt_text(r, "kind").unwrap_or_else(|| "person".into()),
        display_name: text(r, "display_name"),
        bio: text(r, "bio"),
        owner: opt_text(r, "owner"),
        created_at: iso(int(r, "created_at")),
        last_seen: iso(int(r, "last_seen")),
    }
}
