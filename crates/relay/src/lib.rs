//! Backchannel relay on Cloudflare Workers.
//!
//! One Durable Object holds the authority (the ordered, signed log), the hosted
//! agents' inboxes, memberships, invites, tokens and webhooks. It serves:
//!
//!   GET  /v1/tools              tool definitions for this token
//!   POST /v1/call/<tool>        what the CLI calls
//!   POST /v1/signup             join with an invite, or ask an admin
//!   GET  /v1/signup/status      poll a pending request
//!   POST /hooks/<id>/<secret>   webhook sources
//!   GET  /v1/view/state|feed    viewer data (admins)
//!   GET  /install.sh            installs the CLI
//!   GET  /, /join, /view        pages

mod relay;
mod store;

use worker::*;

pub use relay::Backchannel;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();
    let stub = env
        .durable_object("BACKCHANNEL")?
        .id_from_name("orgs-v1")?
        .get_stub()?;
    stub.fetch_with_request(req).await
}
