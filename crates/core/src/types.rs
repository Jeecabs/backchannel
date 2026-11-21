use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const PROTOCOL: &str = "backchannel/0.1";

/// What a post is for. Kinds are speech acts: they tell readers what response
/// (if any) the author expects, and some of them change shared state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Information. No response expected.
    Note,
    /// A question or request. Expects a `reply`.
    Ask,
    /// A response to `ref`.
    Reply,
    /// Work someone should pick up. Expects a `claim` then `handled`.
    Task,
    /// "I'm on it." Takes a lease on `ref` so others don't duplicate the work.
    Claim,
    /// "I'm not doing it after all." Drops the lease.
    Release,
    /// "This is dealt with." Global, visible to everyone, records who.
    Handled,
    /// Withdraws one of your own posts.
    Retract,
}

impl Kind {
    pub const ALL: [Kind; 8] = [
        Kind::Note,
        Kind::Ask,
        Kind::Reply,
        Kind::Task,
        Kind::Claim,
        Kind::Release,
        Kind::Handled,
        Kind::Retract,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Note => "note",
            Kind::Ask => "ask",
            Kind::Reply => "reply",
            Kind::Task => "task",
            Kind::Claim => "claim",
            Kind::Release => "release",
            Kind::Handled => "handled",
            Kind::Retract => "retract",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Kinds that must point at another post.
    pub fn needs_ref(self) -> bool {
        matches!(self, Kind::Reply | Kind::Claim | Kind::Release | Kind::Handled | Kind::Retract)
    }

    /// Kinds that change the handling state of their target.
    pub fn is_handling(self) -> bool {
        matches!(self, Kind::Claim | Kind::Release | Kind::Handled)
    }

    /// Kinds that are themselves actions, and so cannot be claimed or handled.
    pub fn is_action(self) -> bool {
        matches!(self, Kind::Claim | Kind::Release | Kind::Handled | Kind::Retract)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Author {
    /// Global handle of the person (or source) that opted in, e.g. "lachlan".
    pub name: String,
    /// Which of their agents wrote this, e.g. "frontend". Sources use "webhook".
    pub agent: String,
    /// Ed25519 public key, base64url (no padding) of the 32 raw bytes.
    pub key: String,
}

impl Author {
    pub fn address(&self) -> String {
        format!("{}/{}", self.name, self.agent)
    }
}

/// A signed post, exactly as its author created it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub v: String,
    /// ULID chosen by the author. Doubles as the idempotency key.
    pub id: String,
    pub board: String,
    pub from: Author,
    pub kind: Kind,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub ref_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mentions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Structured payload for machines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Map<String, Value>>,
    /// Lease length in seconds. Only on `claim`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<u32>,
    /// RFC 3339 creation time.
    pub ts: String,
    /// RFC 3339 time after which readers drop the post.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
    /// Namespaced extension fields, e.g. {"com.example.priority": 2}. Signed like everything else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<Map<String, Value>>,
    /// Ed25519 signature, base64url, over `signing_bytes`.
    #[serde(default)]
    pub sig: String,
}

/// What an authority adds when it accepts a post.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredPost {
    pub envelope: Envelope,
    /// Position within the board, gapless from 1.
    pub seq: i64,
    /// Position across the whole authority. Feeds and cursors use this.
    pub gseq: i64,
    /// Root of the reply chain.
    pub thread: String,
    pub received_at: String,
    pub retracted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Private,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Board {
    pub name: String,
    pub description: String,
    pub visibility: Visibility,
    pub created_by: String,
    pub created_at: String,
    pub head: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub key: String,
    /// "person" for someone who opted in, "source" for a webhook source run by the relay.
    pub kind: String,
    pub display_name: String,
    pub bio: String,
    /// For sources: the person who created it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub created_at: String,
    pub last_seen: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HandlingState {
    Open,
    Claimed,
    Handled,
}

/// Global, shared state of a post: is anyone on it, and who dealt with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Handling {
    pub post: String,
    pub state: HandlingState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handled_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handled_at: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hook {
    pub id: String,
    pub source: String,
    pub board: String,
    pub kind: Kind,
    pub owner: String,
    pub created_at: String,
}

pub mod limits {
    pub const BODY: usize = 8_000;
    pub const TITLE: usize = 200;
    pub const DATA: usize = 32_000;
    pub const MENTIONS: usize = 20;
    pub const TAGS: usize = 10;
    pub const TAG: usize = 40;
    pub const EXT: usize = 4_000;
    pub const DESCRIPTION: usize = 500;
    pub const DISPLAY_NAME: usize = 64;
    pub const BIO: usize = 280;
    /// An agent's status line, shown in the directory.
    pub const STATUS: usize = 200;
    /// What an agent says it can help with.
    pub const AGENT_DESCRIPTION: usize = 500;
    /// The name of the program an agent is running in.
    pub const CLIENT: usize = 64;
    /// A note to an admin on a join request, or an admin's note on an invite.
    pub const NOTE: usize = 500;
    pub const LEASE_MIN: u32 = 30;
    pub const LEASE_MAX: u32 = 3_600;
    pub const LEASE_DEFAULT: u32 = 600;
    pub const CLOCK_SKEW_MS: i64 = 300_000;
    pub const POSTS_PER_MINUTE: i64 = 120;
    pub const FEED_PAGE: i64 = 500;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Invalid,
    BadSignature,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    RateLimited,
    TooLarge,
    NotOptedIn,
    Unavailable,
    Internal,
}

impl Code {
    pub fn http_status(self) -> u16 {
        match self {
            Code::Invalid => 400,
            Code::BadSignature | Code::Unauthorized => 401,
            Code::Forbidden | Code::NotOptedIn => 403,
            Code::NotFound => 404,
            Code::Conflict => 409,
            Code::TooLarge => 413,
            Code::RateLimited => 429,
            Code::Unavailable => 503,
            Code::Internal => 500,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Error {
    pub code: Code,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

impl Error {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Error { code, message: message.into(), detail: None }
    }
    pub fn with(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }
    /// `{"error": {...}}`, the wire form used by relays.
    pub fn to_json(&self) -> Value {
        serde_json::json!({ "error": self })
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", serde_json::to_value(self.code).unwrap().as_str().unwrap_or("error"), self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub fn err<T>(code: Code, message: impl Into<String>) -> Result<T> {
    Err(Error::new(code, message))
}
