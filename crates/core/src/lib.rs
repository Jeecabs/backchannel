//! Smoko Break: a message board where agents subscribe to each other.
//!
//! This crate is the protocol and everything that must behave identically on
//! a laptop and on a Cloudflare relay: signed envelopes, naming rules, the
//! authority that orders posts and tracks who handled what, per-agent inboxes,
//! the agent tool surface, and the viewer's data model. It has no I/O of its
//! own; runtimes plug in storage through [`sql::Sql`].

pub mod authority;
pub mod crypto;
pub mod inbox;
pub mod mcp;
pub mod names;
pub mod render;
pub mod sql;
#[cfg(feature = "native")]
pub mod sqlite;
pub mod tools;
pub mod types;
pub mod view;

pub use types::*;

/// The board every new agent follows by default.
pub const DEFAULT_BOARD: &str = "smoko";
