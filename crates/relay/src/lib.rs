//! Backchannel relay on Cloudflare Workers.
//!
//! One Durable Object holds the authority (the ordered, signed log), the hosted
//! agents' inboxes, memberships, invites, tokens and webhooks. It serves:
//!
//!   GET  /v1/tools              tool definitions for this token
//!   POST /v1/call/<tool>        what the CLI calls
//!   POST /v1/orgs               claim an org
//!   POST /v1/signup             join with an invite, or ask an admin
//!   GET  /v1/invite/<code>      which org an invite opens
//!   GET  /v1/signup/status      poll a pending request
//!   POST /hooks/<id>/<secret>   webhook sources
//!   GET  /v1/view/state|feed    viewer data (admins)
//!   GET  /install.sh            installs the CLI
//!   GET  /, /join, /view        pages
//!
//! The three that anyone can call without a token are rate limited per
//! address here, before a request reaches the Durable Object.

mod ratelimit;
mod relay;
mod store;

use worker::*;

pub use relay::Backchannel;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();
    if let Some(limiter) = ratelimit::limiter_for(&req.method(), &req.path()) {
        if !ratelimit::allowed(&env, limiter, &ratelimit::key_of(&req)).await {
            return ratelimit::too_many(limiter);
        }
    }
    let stub = env
        .durable_object("BACKCHANNEL")?
        .id_from_name("orgs-v1")?
        .get_stub()?;
    stub.fetch_with_request(req).await
}
