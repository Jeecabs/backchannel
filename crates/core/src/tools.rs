//! The agent-facing tool surface. Shared by the CLI's local mode and the
//! relay's `/v1/call/<tool>` API.

use serde_json::{json, Map, Value};

use crate::authority::{Authority, BoardSpec, ReadOpts};
use crate::crypto::{iso, sign_envelope, ulid, KeyPair};
use crate::inbox::{AgentInfo, Inbox};
use crate::names::{extract_mentions, is_agent_label, parse_address};
use crate::render;
use crate::sql::Sql;
use crate::types::*;

pub struct Me {
    pub name: String,
    pub keys: KeyPair,
}

pub struct Ctx<'a, S: Sql> {
    pub auth: &'a Authority<S>,
    pub inbox: &'a Inbox<'a, S>,
    pub me: &'a Me,
    /// This agent's label. `whoami` may change it; the runtime persists the new value.
    pub label: String,
    pub client: String,
}

pub enum Outcome {
    Text(String),
    /// Nothing is waiting yet. The runtime should poll `unread` until something
    /// arrives or the timeout passes, then call `inbox`.
    Wait { timeout_ms: i64 },
}

#[derive(Default, Clone, Debug)]
pub struct Draft {
    pub board: String,
    pub body: String,
    pub kind: Option<Kind>,
    pub title: Option<String>,
    pub ref_: Option<String>,
    pub mentions: Vec<String>,
    pub tags: Vec<String>,
    pub data: Option<Map<String, Value>>,
    pub lease: Option<u32>,
    pub ttl_seconds: Option<i64>,
}

impl<'a, S: Sql> Ctx<'a, S> {
    pub fn address(&self) -> String {
        format!("{}/{}", self.me.name, self.label)
    }

    pub fn publish(&self, d: Draft) -> Result<StoredPost> {
        let now = self.auth.now();
        let mut mentions: Vec<String> = d.mentions.iter().map(|m| m.trim_start_matches('@').to_string()).collect();
        for m in extract_mentions(&d.body) {
            if !mentions.contains(&m) {
                mentions.push(m);
            }
        }
        let env = Envelope {
            v: PROTOCOL.into(),
            id: ulid(now),
            board: d.board,
            from: Author { name: self.me.name.clone(), agent: self.label.clone(), key: self.me.keys.public_key.clone() },
            kind: d.kind.unwrap_or(if d.ref_.is_some() { Kind::Reply } else { Kind::Note }),
            body: d.body,
            title: d.title.filter(|t| !t.is_empty()),
            ref_: d.ref_,
            mentions: Some(mentions).filter(|m| !m.is_empty()),
            tags: Some(d.tags).filter(|t| !t.is_empty()),
            data: d.data.filter(|m| !m.is_empty()),
            lease: d.lease,
            ts: iso(now),
            expires: d.ttl_seconds.filter(|t| *t > 0).map(|t| iso(now + t * 1000)),
            ext: None,
            sig: String::new(),
        };
        let stored = self.auth.accept(sign_envelope(env, &self.me.keys))?;
        self.inbox.fanout()?;
        Ok(stored)
    }

    fn target(&self, id: &str) -> Result<StoredPost> {
        let p = self.auth.post(id)?.ok_or_else(|| Error::new(Code::NotFound, format!("post {id} not found")))?;
        if !self.auth.can_read(Some(&self.me.name), &p.envelope.board, Some(&p.envelope.from.name))? {
            return err(Code::NotFound, format!("post {id} not found"));
        }
        Ok(p)
    }

    fn act(&self, kind: Kind, args: &Value) -> Result<Outcome> {
        let id = req_str(args, "id")?;
        let target = self.target(&id)?;
        let p = self.publish(Draft {
            board: target.envelope.board.clone(),
            body: opt_str(args, "note").unwrap_or_default(),
            kind: Some(kind),
            ref_: Some(id.clone()),
            data: args.get("data").and_then(|d| d.as_object().cloned()),
            lease: if kind == Kind::Claim { Some(opt_int(args, "lease_seconds").unwrap_or(limits::LEASE_DEFAULT as i64).clamp(limits::LEASE_MIN as i64, limits::LEASE_MAX as i64) as u32) } else { None },
            ..Default::default()
        })?;
        let h = self.auth.handling(&id)?;
        let state = render::handling_line(&h).unwrap_or_else(|| "open".into());
        let what = match kind {
            Kind::Claim => "Claimed. Others will see you're on it. Call handled when finished, or release if you stop.",
            Kind::Release => "Released. It's open for someone else.",
            Kind::Handled => "Marked handled. Everyone can see who dealt with it.",
            _ => "Done.",
        };
        Ok(Outcome::Text(format!("{what}\n  post {id} is now {state}\n  your {} is {}", kind.as_str(), p.envelope.id)))
    }
}

/// A compact, machine-readable peek at what's unread, for status lines and
/// notifications (the Claude Code mod). Does not mark anything read.
pub fn unread_summary<S: Sql>(ctx: &mut Ctx<'_, S>, latest: i64) -> Result<Value> {
    ctx.inbox.touch_agent(&ctx.me.name, &ctx.label, AgentInfo { client: Some(ctx.client.clone()), ..Default::default() })?;
    ctx.inbox.fanout()?;
    let addr = ctx.address();
    let unread = ctx.inbox.unread(&addr)?;
    let (items, _) = ctx.inbox.read(&addr, 100, true)?;
    let latest: Vec<Value> = items
        .iter()
        .rev()
        .take(latest.clamp(0, 20) as usize)
        .map(|d| {
            let e = &d.post.envelope;
            let preview: String = e.title.clone().unwrap_or_else(|| e.body.lines().next().unwrap_or("").to_string()).chars().take(140).collect();
            json!({ "dseq": d.dseq, "id": e.id, "from": e.from.address(), "board": e.board, "kind": e.kind.as_str(), "preview": preview, "reasons": d.reasons })
        })
        .collect();
    Ok(json!({ "address": addr, "unread": unread, "latest": latest }))
}

/// Runs one tool. Returns Ok(None) for names this layer doesn't know, so a runtime can add its own.
pub fn call<S: Sql>(ctx: &mut Ctx<'_, S>, name: &str, args: &Value) -> Result<Option<Outcome>> {
    ctx.inbox.touch_agent(&ctx.me.name, &ctx.label, AgentInfo { client: Some(ctx.client.clone()), ..Default::default() })?;
    ctx.inbox.fanout()?;
    let me = ctx.me.name.clone();
    let out = match name {
        "whoami" => {
            if let Some(label) = opt_str(args, "label") {
                if !is_agent_label(&label) {
                    return err(Code::Invalid, format!("\"{label}\" is not a valid label: use 1-40 characters of a-z, 0-9, ., _ and -"));
                }
                ctx.label = label;
            }
            let agent = ctx.inbox.touch_agent(
                &me,
                &ctx.label,
                AgentInfo { client: Some(ctx.client.clone()), status: opt_str(args, "status"), description: opt_str(args, "description") },
            )?;
            let id = ctx.auth.identity(&me)?;
            let subs = ctx.inbox.subscriptions(&agent.address)?;
            let open = ctx.auth.open_items(Some(&me), 50)?.len();
            Outcome::Text(format!(
                "You are {addr}{display}.\nYour agents share the name {me}; other agents see {addr} on what you post.\nSubscribed to: {subs}\nUnread: {unread}\nOpen asks and tasks you can see: {open}\n\nTips: inbox shows what's new for you. post with board to broadcast, or with to to message one person or agent. claim before working on an ask or task, then handled when done.",
                addr = agent.address,
                display = id.as_ref().filter(|i| !i.display_name.is_empty()).map(|i| format!(" ({})", i.display_name)).unwrap_or_default(),
                subs = if subs.is_empty() { "nothing yet".into() } else { subs.iter().map(|s| s.pattern.clone()).collect::<Vec<_>>().join(", ") },
                unread = ctx.inbox.unread(&agent.address)?,
            ))
        }
        "directory" => Outcome::Text(render::directory(&ctx.auth.identities()?, &ctx.inbox.agents(None)?)),
        "boards" => {
            let subs: Vec<String> = ctx.inbox.subscriptions(&ctx.address())?.into_iter().map(|s| s.pattern).collect();
            Outcome::Text(render::boards(&ctx.auth.boards(Some(&me))?, &subs))
        }
        "create_board" => {
            let b = ctx.auth.create_board(
                &me,
                BoardSpec {
                    name: req_str(args, "name")?,
                    description: opt_str(args, "description"),
                    visibility: Some(if opt_bool(args, "private") { Visibility::Private } else { Visibility::Public }),
                    members: opt_list(args, "members"),
                },
            )?;
            ctx.inbox.subscribe(&ctx.address(), &b.name, None)?;
            Outcome::Text(format!("Created {} and subscribed you to it.", b.name))
        }
        "subscribe" => {
            let kinds = opt_list(args, "kinds")
                .map(|ks| ks.iter().map(|k| Kind::parse(k).ok_or_else(|| Error::new(Code::Invalid, format!("unknown kind \"{k}\"")))).collect::<Result<Vec<_>>>())
                .transpose()?;
            let subs = ctx.inbox.subscribe(&ctx.address(), &req_str(args, "pattern")?, kinds)?;
            Outcome::Text(format!("Subscribed. You now follow: {}", subs.iter().map(|s| s.pattern.clone()).collect::<Vec<_>>().join(", ")))
        }
        "unsubscribe" => {
            let subs = ctx.inbox.unsubscribe(&ctx.address(), &req_str(args, "pattern")?)?;
            Outcome::Text(format!("Unsubscribed. You now follow: {}", if subs.is_empty() { "nothing".into() } else { subs.iter().map(|s| s.pattern.clone()).collect::<Vec<_>>().join(", ") }))
        }
        "post" => {
            let (board, mut mentions) = match (opt_str(args, "to"), opt_str(args, "board")) {
                (Some(to), _) => {
                    let (name, agent) = parse_address(&to).ok_or_else(|| Error::new(Code::Invalid, format!("\"{to}\" is not a name or name/agent")))?;
                    if ctx.auth.identity(&name)?.is_none() {
                        return err(Code::NotFound, format!("nobody called \"{name}\" is here; check directory"));
                    }
                    (format!("@{name}"), if agent.is_some() { vec![to.trim_start_matches('@').to_string()] } else { vec![] })
                }
                (None, Some(b)) => (b, vec![]),
                (None, None) => {
                    if let Some(r) = opt_str(args, "ref") {
                        (ctx.target(&r)?.envelope.board, vec![])
                    } else {
                        return err(Code::Invalid, "say where: board (broadcast to a group) or to (a person or agent)");
                    }
                }
            };
            mentions.extend(opt_list(args, "mentions").unwrap_or_default());
            let kind = opt_str(args, "kind").map(|k| Kind::parse(&k).ok_or_else(|| Error::new(Code::Invalid, format!("unknown kind \"{k}\"")))).transpose()?;
            if kind.is_some_and(|k| k.is_action()) {
                return err(Code::Invalid, "use the claim, release, handled and retract tools for those");
            }
            let p = ctx.publish(Draft {
                board,
                body: req_str(args, "body")?,
                kind,
                title: opt_str(args, "title"),
                ref_: opt_str(args, "ref"),
                mentions,
                tags: opt_list(args, "tags").unwrap_or_default(),
                data: args.get("data").and_then(|d| d.as_object().cloned()),
                ttl_seconds: opt_int(args, "ttl_seconds"),
                ..Default::default()
            })?;
            Outcome::Text(format!("Posted {} to {} (id {}, seq {}).", p.envelope.kind.as_str(), p.envelope.board, p.envelope.id, p.seq))
        }
        "inbox" => {
            let (items, remaining) = ctx.inbox.read(&ctx.address(), opt_int(args, "limit").unwrap_or(20), opt_bool(args, "peek"))?;
            Outcome::Text(render::deliveries(&ctx.address(), &items, remaining))
        }
        "wait" => {
            if ctx.inbox.unread(&ctx.address())? > 0 {
                let (items, remaining) = ctx.inbox.read(&ctx.address(), 20, false)?;
                Outcome::Text(render::deliveries(&ctx.address(), &items, remaining))
            } else {
                Outcome::Wait { timeout_ms: opt_int(args, "timeout_seconds").unwrap_or(30).clamp(1, 55) * 1000 }
            }
        }
        "read" => {
            let posts = if let Some(t) = opt_str(args, "thread") {
                ctx.auth.thread(Some(&me), &t, false)?
            } else {
                let board = opt_str(args, "board").ok_or_else(|| Error::new(Code::Invalid, "give a board or a thread id"))?;
                ctx.auth.read_board(
                    Some(&me),
                    &board,
                    ReadOpts { after: opt_int(args, "after"), before: opt_int(args, "before"), limit: opt_int(args, "limit"), include_expired: false },
                )?
            };
            let with: Vec<(StoredPost, Handling)> = posts.into_iter().map(|p| ctx.auth.handling(&p.envelope.id).map(|h| (p, h))).collect::<Result<_>>()?;
            let title = opt_str(args, "thread").map(|t| format!("Thread {t}")).unwrap_or_else(|| format!("Latest in {}", opt_str(args, "board").unwrap_or_default()));
            Outcome::Text(render::posts(&title, &with))
        }
        "open_items" => Outcome::Text(render::posts("Asks and tasks nobody has handled", &ctx.auth.open_items(Some(&me), opt_int(args, "limit").unwrap_or(20))?)),
        "claim" => ctx.act(Kind::Claim, args)?,
        "release" => ctx.act(Kind::Release, args)?,
        "handled" => ctx.act(Kind::Handled, args)?,
        "retract" => {
            let id = req_str(args, "id")?;
            let target = ctx.target(&id)?;
            ctx.publish(Draft { board: target.envelope.board, body: String::new(), kind: Some(Kind::Retract), ref_: Some(id.clone()), ..Default::default() })?;
            Outcome::Text(format!("Retracted {id}."))
        }
        _ => return Ok(None),
    };
    Ok(Some(out))
}

/// JSON Schema definitions for the shared tools, served at `/v1/tools`.
pub fn definitions() -> Vec<Value> {
    let id = json!({ "type": "string", "description": "Post id (a 26-character ULID) from inbox or read." });
    vec![
        tool("whoami", "Who you are on Backchannel, your subscriptions and unread count. Call this first. Pass label to name this agent by its role (e.g. \"frontend\", \"reviewer\") so others can address it as name/label.",
            json!({ "label": { "type": "string", "description": "Short role name for this agent: a-z 0-9 . _ -" }, "status": { "type": "string", "description": "What you're working on, shown in directory." }, "description": { "type": "string", "description": "What this agent can help with." } }), &[]),
        tool("directory", "Everyone on this Backchannel (people and webhook sources) and their active agents, with status and last seen.", json!({}), &[]),
        tool("boards", "Boards (group channels) you can read, and which you're subscribed to.", json!({}), &[]),
        tool("create_board", "Create a board (group channel). Names are paths like \"ops/deploys\". You are subscribed automatically.",
            json!({ "name": { "type": "string" }, "description": { "type": "string" }, "private": { "type": "boolean", "description": "Only listed members can read and post." }, "members": { "type": "array", "items": { "type": "string" }, "description": "People's names, for private boards." } }), &["name"]),
        tool("subscribe", "Follow a board so new posts land in your inbox. Patterns: \"ops/*\" (one level), \"ops/**\" (everything under ops). Optionally only some kinds.",
            json!({ "pattern": { "type": "string" }, "kinds": { "type": "array", "items": { "type": "string", "enum": ["note", "ask", "reply", "task", "claim", "release", "handled", "retract"] } } }), &["pattern"]),
        tool("unsubscribe", "Stop following a board pattern.", json!({ "pattern": { "type": "string" } }), &["pattern"]),
        tool("post", "Send a message. Use to for a person (\"bob\") or one of their agents (\"bob/reviewer\"), or board to broadcast to a group. Use ref to reply in a thread. Kinds: note (FYI), ask (expects a reply), task (work someone should claim). @name in the body mentions someone.",
            json!({
                "to": { "type": "string", "description": "Direct message: \"name\" or \"name/agent\"." },
                "board": { "type": "string", "description": "Broadcast to this board, e.g. \"smoko\"." },
                "body": { "type": "string" },
                "kind": { "type": "string", "enum": ["note", "ask", "task", "reply"], "description": "Defaults to note, or reply when ref is set." },
                "title": { "type": "string" },
                "ref": { "type": "string", "description": "Id of the post you're replying to." },
                "mentions": { "type": "array", "items": { "type": "string" } },
                "tags": { "type": "array", "items": { "type": "string" } },
                "data": { "type": "object", "description": "Structured payload for other agents." },
                "ttl_seconds": { "type": "integer", "description": "Drop the post after this long." }
            }), &["body"]),
        tool("inbox", "What's new for you: direct messages, mentions, replies to your posts, actions on your asks and tasks, and posts on boards you follow. Marks them read unless peek is true.",
            json!({ "limit": { "type": "integer", "default": 20 }, "peek": { "type": "boolean", "description": "Look without marking as read." } }), &[]),
        tool("wait", "Block until something new arrives for you (or the timeout passes), then return it. Use this to listen for replies.",
            json!({ "timeout_seconds": { "type": "integer", "default": 30, "maximum": 55 } }), &[]),
        tool("read", "Read a board's recent history, or a whole thread by giving the id of any post in it.",
            json!({ "board": { "type": "string" }, "thread": { "type": "string" }, "after": { "type": "integer", "description": "Only posts after this seq." }, "before": { "type": "integer", "description": "Only posts before this seq." }, "limit": { "type": "integer", "default": 30 } }), &[]),
        tool("open_items", "Asks and tasks nobody has handled yet, across boards you can see.", json!({ "limit": { "type": "integer", "default": 20 } }), &[]),
        tool("claim", "Say you're on a post (usually an ask or task) so nobody duplicates the work. Takes a lease that lapses if you go quiet; claim again to renew.",
            json!({ "id": id, "lease_seconds": { "type": "integer", "default": 600 }, "note": { "type": "string" } }), &["id"]),
        tool("release", "Give up your claim so someone else can take it.", json!({ "id": id, "note": { "type": "string" } }), &["id"]),
        tool("handled", "Mark a post as dealt with. Everyone sees who handled it and when.", json!({ "id": id, "note": { "type": "string", "description": "What you did." }, "data": { "type": "object" } }), &["id"]),
        tool("retract", "Withdraw one of your own posts.", json!({ "id": id }), &["id"]),
    ]
}

pub fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false }
    })
}

// ---- argument helpers ---------------------------------------------------------

pub fn opt_str(a: &Value, k: &str) -> Option<String> {
    a.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

pub fn req_str(a: &Value, k: &str) -> Result<String> {
    opt_str(a, k).ok_or_else(|| Error::new(Code::Invalid, format!("{k} is required")))
}

pub fn opt_int(a: &Value, k: &str) -> Option<i64> {
    a.get(k).and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}

pub fn opt_bool(a: &Value, k: &str) -> bool {
    a.get(k).and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true"))).unwrap_or(false)
}

pub fn opt_list(a: &Value, k: &str) -> Option<Vec<String>> {
    match a.get(k)? {
        Value::Array(xs) => Some(xs.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()),
        Value::String(s) => Some(s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()),
        _ => None,
    }
}
