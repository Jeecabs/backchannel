//! Rate limits on the endpoints that need no token.
//!
//! Claiming an org, signing up and testing an invite code are open to anyone,
//! so each address gets a small budget per minute for each. The counting is
//! done by Cloudflare's rate limiting bindings, declared in wrangler.toml,
//! where the numbers live. A relay deployed without a binding simply isn't
//! limited on that endpoint: the check is skipped, never failed.

use std::net::IpAddr;

use backchannel_core::{Code, Error};
use serde_json::json;
use worker::{console_error, Env, Method, Request, Response, Result};

use crate::relay::{cors, json_error};

/// One budget: the binding that counts it, and the window the binding is
/// configured with in wrangler.toml, which is what a caller is told to wait.
#[derive(Debug, PartialEq, Eq)]
pub struct Limiter {
    pub binding: &'static str,
    pub window_seconds: u32,
}

/// Claiming an org name: `POST /v1/orgs`.
pub const ORGS: Limiter = Limiter {
    binding: "ORG_LIMITER",
    window_seconds: 60,
};
/// Joining with an invite, or asking an org's admins: `POST /v1/signup`.
pub const SIGNUPS: Limiter = Limiter {
    binding: "SIGNUP_LIMITER",
    window_seconds: 60,
};
/// Anything that tells a caller whether an invite code is good:
/// `GET /v1/invite/<code>`, and a signup that carries a code.
pub const INVITES: Limiter = Limiter {
    binding: "INVITE_LIMITER",
    window_seconds: 60,
};

/// The budget a request draws from, going by its method and path alone. A
/// signup that carries an invite also draws from `INVITES`; the relay does
/// that itself once it has read the body.
pub fn limiter_for(method: &Method, path: &str) -> Option<&'static Limiter> {
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match (method, segs.as_slice()) {
        (Method::Post, ["v1", "orgs"]) => Some(&ORGS),
        (Method::Post, ["v1", "signup"]) => Some(&SIGNUPS),
        (Method::Get, ["v1", "invite", _]) => Some(&INVITES),
        _ => None,
    }
}

/// What requests are counted under: the client's address as Cloudflare reports
/// it. An IPv6 address is counted by its /64, because one subscriber usually
/// holds a whole /64 and could otherwise use a fresh address for every request.
pub fn client_key(address: Option<&str>) -> String {
    match address
        .map(str::trim)
        .and_then(|a| a.parse::<IpAddr>().ok())
    {
        Some(IpAddr::V4(v4)) => v4.to_string(),
        Some(IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
        // Cloudflare always sends an address. Without one, everyone shares a budget.
        None => "unknown".to_string(),
    }
}

/// The key for the request in hand.
pub fn key_of(req: &Request) -> String {
    client_key(
        req.headers()
            .get("cf-connecting-ip")
            .ok()
            .flatten()
            .as_deref(),
    )
}

/// Counts one request against `limiter`. False means the address has used up
/// its budget for this window.
///
/// With no binding of that name, there is no limit: whoever deployed this
/// relay left it out, and the relay must still work. If a binding is there but
/// can't be used, the request is let through and the fault is logged, since
/// locking everyone out of signing up would be the worse failure.
pub async fn allowed(env: &Env, limiter: &Limiter, key: &str) -> bool {
    let binding = match env.rate_limiter(limiter.binding) {
        Ok(b) => b,
        Err(e) => {
            if !e.to_string().contains("is undefined") {
                console_error!("rate limit binding {} can't be used: {e}", limiter.binding);
            }
            return true;
        }
    };
    match binding.limit(key.to_string()).await {
        Ok(outcome) => outcome.success,
        Err(e) => {
            console_error!("rate limit check on {} failed: {e}", limiter.binding);
            true
        }
    }
}

/// The error an address over its budget gets.
pub fn slow_down(limiter: &Limiter) -> Error {
    Error::new(
        Code::RateLimited,
        "Slow down: too many tries from your address. Try again in a minute.",
    )
    .with(json!({ "retry_after": limiter.window_seconds }))
}

/// The 429 response for it. `json_error` adds the Retry-After header.
pub fn too_many(limiter: &Limiter) -> Result<Response> {
    cors(json_error(&slow_down(limiter))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_open_endpoint_has_its_budget() {
        assert_eq!(limiter_for(&Method::Post, "/v1/orgs"), Some(&ORGS));
        assert_eq!(limiter_for(&Method::Post, "/v1/orgs/"), Some(&ORGS));
        assert_eq!(limiter_for(&Method::Post, "/v1/signup"), Some(&SIGNUPS));
        assert_eq!(
            limiter_for(&Method::Get, "/v1/invite/CHAN-7KQ2-M9XD-4TRW"),
            Some(&INVITES)
        );
        // Pages, polling for approval, and everything that needs a token or a secret URL are not limited here.
        for (method, path) in [
            (Method::Get, "/"),
            (Method::Get, "/join"),
            (Method::Get, "/v1/signup/status"),
            (Method::Get, "/v1/orgs"),
            (Method::Post, "/v1/call/post"),
            (Method::Post, "/hooks/abc/def"),
            (Method::Get, "/v1/view/feed"),
            (Method::Get, "/install.sh"),
        ] {
            assert_eq!(limiter_for(&method, path), None, "{path}");
        }
    }

    #[test]
    fn requests_are_counted_by_address() {
        assert_eq!(client_key(Some("203.0.113.7")), "203.0.113.7");
        assert_eq!(client_key(Some(" 203.0.113.7 ")), "203.0.113.7");
        assert_ne!(
            client_key(Some("203.0.113.7")),
            client_key(Some("203.0.113.8"))
        );
        // One /64 is one subscriber, however many addresses they use inside it.
        assert_eq!(
            client_key(Some("2001:db8:1:2:aaaa:bbbb:cccc:dddd")),
            "2001:db8:1:2::/64"
        );
        assert_eq!(
            client_key(Some("2001:db8:1:2::1")),
            client_key(Some("2001:db8:1:2:ffff:ffff:ffff:ffff"))
        );
        assert_ne!(
            client_key(Some("2001:db8:1:2::1")),
            client_key(Some("2001:db8:1:3::1"))
        );
        assert_eq!(client_key(Some("::ffff:203.0.113.7")), "203.0.113.7");
        // No address, or nonsense: one shared budget, not a free pass.
        assert_eq!(client_key(None), "unknown");
        assert_eq!(client_key(Some("not an address")), "unknown");
    }

    #[test]
    fn over_the_limit_is_a_429_that_says_when_to_come_back() {
        let e = slow_down(&ORGS);
        assert_eq!(e.code, Code::RateLimited);
        assert_eq!(e.code.http_status(), 429);
        assert_eq!(e.detail.as_ref().unwrap()["retry_after"], 60);
        assert_eq!(crate::relay::retry_after(&e), Some(60));
        assert_eq!(e.to_json()["error"]["code"], "rate_limited");
        assert!(e.message.contains("Try again in a minute"));
    }
}
