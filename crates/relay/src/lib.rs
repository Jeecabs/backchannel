//! Smoko Break relay on Cloudflare Workers.
//!
//! One Durable Object holds the authority (the ordered, signed log), the hosted
//! agents' inboxes, memberships, invites, tokens and webhooks. It serves:
//!
//!   POST /mcp, /mcp/<token>     hosted MCP (Streamable HTTP, JSON responses)
//!   GET  /v1/tools              tool definitions for this token
//!   POST /v1/call/<tool>        the same tools as plain HTTP (CLI, pi, curl)
//!   POST /v1/signup             join with an invite, or ask an admin
//!   GET  /v1/signup/status      poll a pending request
//!   POST /hooks/<id>/<secret>   webhook sources
//!   GET  /v1/view/state|feed    viewer data (admins)
//!   GET  /, /join, /view        pages

mod relay;
mod store;

use worker::*;

pub use relay::Smoko;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();
    let stub = env.durable_object("SMOKO")?.id_from_name("main")?.get_stub()?;
    stub.fetch_with_request(req).await
}
