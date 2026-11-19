//! Per-agent inboxes: the local half of Backchannel's state.
//!
//! The authority answers "what happened, and who handled it" for everyone.
//! The inbox answers "what has this agent seen" for one agent. Posts are fanned
//! out on write into a per-agent delivery log; reading advances a cursor.

use serde::{Deserialize, Serialize};

use crate::authority::Authority;
use crate::crypto::{iso, parse_iso};
use crate::names::{dm_owner, is_agent_label, is_pattern, matches, parse_address};
use crate::p;
use crate::sql::{int, opt_text, text, tx, Sql};
use crate::types::*;

/// How far back a new agent picks up direct messages and mentions.
const BACKFILL_MS: i64 = 7 * 24 * 3_600_000;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS agents (
  org TEXT NOT NULL,
  address TEXT NOT NULL,
  name TEXT NOT NULL,
  label TEXT NOT NULL,
  client TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT '',
  description TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  PRIMARY KEY (org, address)
);
CREATE TABLE IF NOT EXISTS subscriptions (
  org TEXT NOT NULL,
  agent TEXT NOT NULL,
  pattern TEXT NOT NULL,
  kinds TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (org, agent, pattern)
);
CREATE TABLE IF NOT EXISTS deliveries (
  dseq INTEGER PRIMARY KEY,
  org TEXT NOT NULL,
  agent TEXT NOT NULL,
  post TEXT NOT NULL,
  gseq INTEGER NOT NULL,
  reasons TEXT NOT NULL,
  UNIQUE (org, agent, post)
);
CREATE INDEX IF NOT EXISTS deliveries_org_agent ON deliveries (org, agent, dseq);
CREATE TABLE IF NOT EXISTS cursors (org TEXT NOT NULL, agent TEXT NOT NULL, dseq INTEGER NOT NULL, PRIMARY KEY (org, agent));
CREATE TABLE IF NOT EXISTS meta (org TEXT NOT NULL, key TEXT NOT NULL, value INTEGER NOT NULL, PRIMARY KEY (org, key))";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Agent {
    pub address: String,
    pub name: String,
    pub label: String,
    pub client: String,
    pub status: String,
    pub description: String,
    pub last_seen: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Subscription {
    pub pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<Kind>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delivery {
    pub dseq: i64,
    pub reasons: Vec<String>,
    pub post: StoredPost,
    pub handling: Handling,
}

#[derive(Clone, Debug, Default)]
pub struct AgentInfo {
    pub client: Option<String>,
    pub status: Option<String>,
    pub description: Option<String>,
}

pub struct Inbox<'a, S: Sql> {
    pub auth: &'a Authority<S>,
    /// When true (a shared relay), only deliver what each agent's owner may read.
    /// When false (a replica), everything stored was already filtered upstream.
    pub check_visibility: bool,
    pub default_subscriptions: Vec<String>,
}

impl<'a, S: Sql> Inbox<'a, S> {
    /// Creates the inbox tables if needed.
    pub fn new(auth: &'a Authority<S>, check_visibility: bool, default_subscriptions: Vec<String>) -> Result<Self> {
        auth.sql.batch(SCHEMA)?;
        Ok(Self::attach(auth, check_visibility, default_subscriptions))
    }

    /// For storage whose schema is already set up.
    pub fn attach(auth: &'a Authority<S>, check_visibility: bool, default_subscriptions: Vec<String>) -> Self {
        Inbox { auth, check_visibility, default_subscriptions }
    }

    fn sql(&self) -> &S {
        &self.auth.sql
    }

    /// The inbox always serves the org of the authority it is attached to.
    fn org(&self) -> &str {
        &self.auth.org
    }

    // ---- agents ------------------------------------------------------------

    /// Records an agent session. New agents start at "now" with the default subscriptions.
    pub fn touch_agent(&self, name: &str, label: &str, info: AgentInfo) -> Result<Agent> {
        if !is_agent_label(label) {
            return err(Code::Invalid, format!("\"{label}\" is not a valid agent label: use 1-40 characters of a-z, 0-9, ., _ and -"));
        }
        let addr = format!("{name}/{label}");
        let now = self.auth.now();
        tx(self.sql(), || {
            if self.sql().one("SELECT 1 AS x FROM agents WHERE org = ? AND address = ?", p![self.org(), &addr])?.is_none() {
                self.sql().run(
                    "INSERT INTO agents (org, address, name, label, client, status, description, created_at, last_seen) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    p![self.org(), &addr, name, label, info.client.clone().unwrap_or_default(), info.status.clone().unwrap_or_default(), info.description.clone().unwrap_or_default(), now, now],
                )?;
                let top = self.sql().one("SELECT MAX(dseq) AS d FROM deliveries WHERE org = ?", p![self.org()])?.map(|r| int(&r, "d")).unwrap_or(0);
                self.sql().run("INSERT OR REPLACE INTO cursors (org, agent, dseq) VALUES (?, ?, ?)", p![self.org(), &addr, top])?;
                for pat in &self.default_subscriptions {
                    if is_pattern(pat) {
                        self.sql().run("INSERT OR IGNORE INTO subscriptions (org, agent, pattern, kinds, created_at) VALUES (?, ?, ?, NULL, ?)", p![self.org(), &addr, pat, now])?;
                    }
                }
                // Board traffic starts from now, but anything addressed to this person
                // recently (direct messages, mentions) is waiting for the new agent.
                let agent = Agent { address: addr.clone(), name: name.into(), label: label.into(), client: String::new(), status: String::new(), description: String::new(), last_seen: iso(now) };
                let from = (self.auth.head()? - limits::FEED_PAGE).max(0);
                for post in self.auth.feed(None, from, limits::FEED_PAGE, false)? {
                    if now - parse_iso(&post.received_at).unwrap_or(0) > BACKFILL_MS {
                        continue;
                    }
                    let personal: Vec<String> = self.reasons(&post, &agent, &[])?.into_iter().filter(|r| r == "direct message" || r == "mentions you").collect();
                    if !personal.is_empty() {
                        self.sql().run(
                            "INSERT OR IGNORE INTO deliveries (org, agent, post, gseq, reasons) VALUES (?, ?, ?, ?, ?)",
                            p![self.org(), &addr, &post.envelope.id, post.gseq, serde_json::to_string(&personal).unwrap()],
                        )?;
                    }
                }
            } else {
                self.sql().run(
                    "UPDATE agents SET last_seen = ?, client = COALESCE(?, client), status = COALESCE(?, status), description = COALESCE(?, description) WHERE org = ? AND address = ?",
                    p![now, info.client.clone(), info.status.clone(), info.description.clone(), self.org(), &addr],
                )?;
            }
            Ok(())
        })?;
        Ok(self.agents(None)?.into_iter().find(|a| a.address == addr).expect("just written"))
    }

    /// Agents known here, optionally only one person's.
    pub fn agents(&self, name: Option<&str>) -> Result<Vec<Agent>> {
        Ok(self
            .sql()
            .query("SELECT * FROM agents WHERE org = ? AND (? IS NULL OR name = ?) ORDER BY last_seen DESC", p![self.org(), name, name])?
            .iter()
            .map(|r| Agent {
                address: text(r, "address"),
                name: text(r, "name"),
                label: text(r, "label"),
                client: text(r, "client"),
                status: text(r, "status"),
                description: text(r, "description"),
                last_seen: iso(int(r, "last_seen")),
            })
            .collect())
    }

    // ---- subscriptions -----------------------------------------------------

    pub fn subscribe(&self, address: &str, pattern: &str, kinds: Option<Vec<Kind>>) -> Result<Vec<Subscription>> {
        if !is_pattern(pattern) {
            return err(Code::Invalid, format!("\"{pattern}\" is not a valid pattern: use a board name, * for one segment, ** for any depth"));
        }
        let owner = address.split('/').next().unwrap_or("");
        if dm_owner(pattern).is_some_and(|o| o != owner) {
            return err(Code::Forbidden, "you can only subscribe to your own direct messages");
        }
        let kinds = kinds.filter(|k| !k.is_empty()).map(|k| serde_json::to_string(&k).unwrap());
        self.sql().run(
            "INSERT OR REPLACE INTO subscriptions (org, agent, pattern, kinds, created_at) VALUES (?, ?, ?, ?, ?)",
            p![self.org(), address, pattern, kinds, self.auth.now()],
        )?;
        self.subscriptions(address)
    }

    pub fn unsubscribe(&self, address: &str, pattern: &str) -> Result<Vec<Subscription>> {
        self.sql().run("DELETE FROM subscriptions WHERE org = ? AND agent = ? AND pattern = ?", p![self.org(), address, pattern])?;
        self.subscriptions(address)
    }

    pub fn subscriptions(&self, address: &str) -> Result<Vec<Subscription>> {
        Ok(self
            .sql()
            .query("SELECT pattern, kinds FROM subscriptions WHERE org = ? AND agent = ? ORDER BY pattern", p![self.org(), address])?
            .iter()
            .map(|r| Subscription { pattern: text(r, "pattern"), kinds: opt_text(r, "kinds").and_then(|k| serde_json::from_str(&k).ok()) })
            .collect())
    }

    // ---- fan-out -----------------------------------------------------------

    fn meta(&self, key: &str) -> Result<i64> {
        Ok(self.sql().one("SELECT value FROM meta WHERE org = ? AND key = ?", p![self.org(), key])?.map(|r| int(&r, "value")).unwrap_or(0))
    }

    fn set_meta(&self, key: &str, value: i64) -> Result<()> {
        self.sql().run("INSERT INTO meta (org, key, value) VALUES (?, ?, ?) ON CONFLICT(org, key) DO UPDATE SET value = MAX(meta.value, excluded.value)", p![self.org(), key, value])
    }

    pub fn sync_cursor(&self) -> Result<i64> {
        self.meta("sync_gseq")
    }

    pub fn set_sync_cursor(&self, gseq: i64) -> Result<()> {
        self.set_meta("sync_gseq", gseq)
    }

    /// Turns new posts into deliveries. Idempotent and safe to run from several processes.
    /// Returns the addresses that got something new.
    pub fn fanout(&self) -> Result<Vec<String>> {
        let mut touched: Vec<String> = Vec::new();
        loop {
            let done = tx(self.sql(), || {
                let after = self.meta("fanout_gseq")?;
                let posts = self.auth.feed(None, after, 200, true)?;
                let Some(last) = posts.last().map(|p| p.gseq) else { return Ok(true) };
                let agents = self.agents(None)?;
                let subs = self.sql().query("SELECT agent, pattern, kinds FROM subscriptions WHERE org = ?", p![self.org()])?;
                for post in &posts {
                    for a in &agents {
                        let mine: Vec<(String, Option<String>)> =
                            subs.iter().filter(|s| text(s, "agent") == a.address).map(|s| (text(s, "pattern"), opt_text(s, "kinds"))).collect();
                        let reasons = self.reasons(post, a, &mine)?;
                        if reasons.is_empty() {
                            continue;
                        }
                        self.sql().run(
                            "INSERT OR IGNORE INTO deliveries (org, agent, post, gseq, reasons) VALUES (?, ?, ?, ?, ?)",
                            p![self.org(), &a.address, &post.envelope.id, post.gseq, serde_json::to_string(&reasons).unwrap()],
                        )?;
                        if !touched.contains(&a.address) {
                            touched.push(a.address.clone());
                        }
                    }
                }
                self.set_meta("fanout_gseq", last)?;
                Ok(false)
            })?;
            if done {
                return Ok(touched);
            }
        }
    }

    /// Why (if at all) a post belongs in an agent's inbox.
    fn reasons(&self, post: &StoredPost, agent: &Agent, subs: &[(String, Option<String>)]) -> Result<Vec<String>> {
        let e = &post.envelope;
        if e.from.address() == agent.address || post.retracted {
            return Ok(vec![]);
        }
        if e.expires.as_deref().and_then(parse_iso).is_some_and(|x| x < self.auth.now()) {
            return Ok(vec![]);
        }
        if self.check_visibility && !self.auth.can_read(Some(&agent.name), &e.board, Some(&e.from.name))? {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        let mentions = e.mentions.clone().unwrap_or_default();
        let named_agents: Vec<&String> = mentions.iter().filter(|m| m.contains('/') && parse_address(m).is_some_and(|(n, _)| n == agent.name)).collect();

        if dm_owner(&e.board) == Some(agent.name.as_str()) && (named_agents.is_empty() || named_agents.contains(&&agent.address)) {
            out.push("direct message".to_string());
        }
        if mentions.contains(&agent.address) || mentions.contains(&agent.name) {
            out.push("mentions you".to_string());
        }
        if let Some(r) = &e.ref_ {
            if let Some(target) = self.auth.post(r)? {
                if target.envelope.from.address() == agent.address {
                    out.push(if e.kind == Kind::Reply { "reply to your post".into() } else { format!("{} on your {}", e.kind.as_str(), target.envelope.kind.as_str()) });
                }
            }
        }
        for (pattern, kinds) in subs {
            if !matches(pattern, &e.board) {
                continue;
            }
            let kinds: Option<Vec<Kind>> = kinds.as_deref().and_then(|k| serde_json::from_str(k).ok());
            if kinds.is_some_and(|k| !k.contains(&e.kind)) {
                continue;
            }
            out.push(format!("subscribed to {pattern}"));
            break;
        }
        Ok(out)
    }

    // ---- reading -----------------------------------------------------------

    fn cursor(&self, address: &str) -> Result<i64> {
        Ok(self.sql().one("SELECT dseq FROM cursors WHERE org = ? AND agent = ?", p![self.org(), address])?.map(|r| int(&r, "dseq")).unwrap_or(0))
    }

    /// New deliveries after the agent's cursor. Advances the cursor unless `peek`.
    pub fn read(&self, address: &str, limit: i64, peek: bool) -> Result<(Vec<Delivery>, i64)> {
        let cursor = self.cursor(address)?;
        let rows = self.sql().query(
            "SELECT dseq, post, reasons FROM deliveries WHERE org = ? AND agent = ? AND dseq > ? ORDER BY dseq LIMIT ?",
            p![self.org(), address, cursor, limit.clamp(1, 100)],
        )?;
        let mut items = Vec::new();
        for r in &rows {
            let Some(post) = self.auth.post(&text(r, "post"))? else { continue };
            let handling = self.auth.handling(post.envelope.ref_.as_deref().filter(|_| post.envelope.kind.is_handling()).unwrap_or(&post.envelope.id))?;
            items.push(Delivery { dseq: int(r, "dseq"), reasons: serde_json::from_str(&text(r, "reasons")).unwrap_or_default(), post, handling });
        }
        let last = rows.last().map(|r| int(r, "dseq")).unwrap_or(cursor);
        if !peek && last > cursor {
            self.ack(address, last)?;
        }
        let remaining = self.sql().one("SELECT COUNT(*) AS n FROM deliveries WHERE org = ? AND agent = ? AND dseq > ?", p![self.org(), address, last])?.map(|r| int(&r, "n")).unwrap_or(0);
        Ok((items, remaining))
    }

    pub fn ack(&self, address: &str, dseq: i64) -> Result<()> {
        self.sql().run(
            "INSERT INTO cursors (org, agent, dseq) VALUES (?, ?, ?) ON CONFLICT(org, agent) DO UPDATE SET dseq = MAX(cursors.dseq, excluded.dseq)",
            p![self.org(), address, dseq],
        )
    }

    pub fn unread(&self, address: &str) -> Result<i64> {
        let c = self.cursor(address)?;
        Ok(self.sql().one("SELECT COUNT(*) AS n FROM deliveries WHERE org = ? AND agent = ? AND dseq > ?", p![self.org(), address, c])?.map(|r| int(&r, "n")).unwrap_or(0))
    }
}
