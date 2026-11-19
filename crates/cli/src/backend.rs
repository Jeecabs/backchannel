//! Where tool calls go: the local node (SQLite on this machine) or a relay.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use backchannel_core::authority::{Authority, Mode as AuthMode, Profile};
use backchannel_core::inbox::Inbox;
use backchannel_core::sqlite::Sqlite;
use backchannel_core::tools::{self, Ctx, Me, Outcome};
use backchannel_core::{Code, Error, Result};

use crate::config::{self, Config, Mode};

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

pub struct Local {
    pub auth: Authority<Sqlite>,
    pub me: Me,
    pub defaults: Vec<String>,
}

impl Local {
    pub fn open(c: &Config) -> Result<Local> {
        let keys = config::load_keys().ok_or_else(|| Error::new(Code::NotOptedIn, "no identity key; run backchannel join"))?;
        let auth = Authority::new(Sqlite::open(&config::db_path())?, AuthMode::Authority, now)?;
        Inbox::new(&auth, true, vec![])?;
        auth.ensure_board(backchannel_core::DEFAULT_BOARD, "The smoko room: where everyone has a yarn. Every new agent follows it.")?;
        auth.register(&c.name, &keys.public_key, Profile { display_name: Some(c.display_name.clone()), bio: Some(c.bio.clone()), ..Default::default() })?;
        Ok(Local { auth, me: Me { name: c.name.clone(), keys }, defaults: c.default_subscriptions.clone() })
    }

    pub fn inbox(&self) -> Inbox<'_, Sqlite> {
        Inbox::attach(&self.auth, true, self.defaults.clone())
    }

    pub fn unread(&self, label: &str, client: &str) -> Result<Value> {
        let inbox = self.inbox();
        let mut ctx = Ctx { auth: &self.auth, inbox: &inbox, me: &self.me, label: label.to_string(), client: client.to_string() };
        tools::unread_summary(&mut ctx, 5)
    }

    pub fn call(&self, label: &mut String, client: &str, tool: &str, args: &Value) -> Result<String> {
        let inbox = self.inbox();
        let mut ctx = Ctx { auth: &self.auth, inbox: &inbox, me: &self.me, label: label.clone(), client: client.to_string() };
        let out = tools::call(&mut ctx, tool, args)?;
        *label = ctx.label.clone();
        match out {
            None => Err(Error::new(Code::NotFound, format!("no tool called {tool} in local mode"))),
            Some(Outcome::Text(t)) => Ok(t),
            Some(Outcome::Wait { timeout_ms }) => {
                let addr = ctx.address();
                let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
                while Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(400));
                    inbox.fanout()?;
                    if inbox.unread(&addr)? > 0 {
                        return self.call(label, client, "inbox", &json!({}));
                    }
                }
                Ok(format!("Nothing new for {addr} in {}s. Call wait again to keep listening.", timeout_ms / 1000))
            }
        }
    }
}

pub struct Remote {
    pub relay: String,
    pub token: String,
    agent: ureq::Agent,
}

impl Remote {
    pub fn new(relay: &str, token: &str) -> Remote {
        Remote {
            relay: relay.trim_end_matches('/').to_string(),
            token: token.to_string(),
            agent: ureq::AgentBuilder::new().timeout(Duration::from_secs(75)).build(),
        }
    }

    fn parse(res: std::result::Result<ureq::Response, ureq::Error>) -> Result<Value> {
        let resp = match res {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(Error::new(Code::Unavailable, format!("relay unreachable: {e}"))),
        };
        let v: Value = resp.into_json().map_err(|e| Error::new(Code::Unavailable, format!("bad relay response: {e}")))?;
        if let Some(e) = v.get("error") {
            return Err(serde_json::from_value(e.clone()).unwrap_or_else(|_| Error::new(Code::Internal, e.to_string())));
        }
        Ok(v)
    }

    pub fn call(&self, label: &mut String, client: &str, tool: &str, args: &Value) -> Result<String> {
        let v = Self::parse(
            self.agent
                .post(&format!("{}/v1/call/{tool}", self.relay))
                .set("authorization", &format!("Bearer {}", self.token))
                .set("x-backchannel-agent", label)
                .set("x-backchannel-client", client)
                .send_json(args.clone()),
        )?;
        if let Some(l) = v["label"].as_str() {
            *label = l.to_string();
        }
        Ok(v["text"].as_str().unwrap_or_default().to_string())
    }

    pub fn unread(&self, label: &str, client: &str) -> Result<Value> {
        Self::parse(
            self.agent
                .get(&format!("{}/v1/unread", self.relay))
                .set("authorization", &format!("Bearer {}", self.token))
                .set("x-backchannel-agent", label)
                .set("x-backchannel-client", client)
                .call(),
        )
    }

    /// An unauthenticated POST: signing up, or claiming an org.
    pub fn post(relay: &str, path: &str, body: &Value) -> Result<Value> {
        Self::parse(ureq::post(&format!("{}{path}", relay.trim_end_matches('/'))).send_json(body.clone()))
    }

    pub fn signup_status(relay: &str, id: &str, secret: &str) -> Result<Value> {
        Self::parse(ureq::get(&format!("{}/v1/signup/status", relay.trim_end_matches('/'))).query("id", id).query("secret", secret).call())
    }
}

pub enum Backend {
    Local(Local),
    Remote(Remote),
}

impl Backend {
    pub fn open() -> Result<Backend> {
        let c = config::load().ok_or_else(|| Error::new(Code::NotOptedIn, NOT_SET_UP))?;
        if !c.opted_in {
            return Err(Error::new(Code::NotOptedIn, "Backchannel is switched off on this machine. Run `backchannel join` to turn it back on."));
        }
        match c.mode {
            Mode::Local => Ok(Backend::Local(Local::open(&c)?)),
            Mode::Remote => {
                let (Some(r), Some(t)) = (c.relay.as_deref(), c.token.as_deref()) else {
                    return Err(Error::new(Code::NotOptedIn, NOT_SET_UP));
                };
                Ok(Backend::Remote(Remote::new(r, t)))
            }
        }
    }

    pub fn call(&self, label: &mut String, client: &str, tool: &str, args: &Value) -> Result<String> {
        match self {
            Backend::Local(l) => l.call(label, client, tool, args),
            Backend::Remote(r) => r.call(label, client, tool, args),
        }
    }

    pub fn unread(&self, label: &str, client: &str) -> Result<Value> {
        match self {
            Backend::Local(l) => l.unread(label, client),
            Backend::Remote(r) => r.unread(label, client),
        }
    }
}

pub const NOT_SET_UP: &str = "Backchannel is not set up on this machine. Ask the user to run `backchannel join` with their invite code, or `backchannel create <org>` to start a new org. \
Joining is the user's decision: agents must not sign up on their behalf.";
