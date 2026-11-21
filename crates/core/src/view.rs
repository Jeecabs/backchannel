//! Data for the viewer (the "who sent what" moderator screen). Served by the
//! relay behind an admin token, and by `backchannel view` on localhost. Also
//! the response headers for the pages themselves.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::authority::Authority;
use crate::crypto::{fingerprint, verify_envelope};
use crate::inbox::Inbox;
use crate::sql::Sql;
use crate::types::*;

pub fn state<S: Sql>(auth: &Authority<S>, inbox: &Inbox<'_, S>, title: &str, extra: Value) -> Result<Value> {
    let people: Vec<Value> = auth
        .identities()?
        .into_iter()
        .map(|i| {
            let fp = fingerprint(&i.key);
            let mut v = serde_json::to_value(i).unwrap();
            v["fingerprint"] = json!(fp);
            v
        })
        .collect();
    let open: Vec<Value> = auth.open_items(None, 50)?.into_iter().map(|(p, h)| json!({ "post": p, "handling": h })).collect();
    Ok(json!({
        "title": title,
        "protocol": PROTOCOL,
        "head": auth.head()?,
        "boards": auth.boards(None)?,
        "people": people,
        "agents": inbox.agents(None)?,
        "open": open,
        "extra": extra,
    }))
}

pub fn feed<S: Sql>(auth: &Authority<S>, after: i64, limit: i64) -> Result<Value> {
    let posts: Vec<Value> = auth
        .feed(None, after, limit, true)?
        .into_iter()
        .map(|p| {
            let target = if p.envelope.kind.is_handling() { p.envelope.ref_.clone() } else { Some(p.envelope.id.clone()) };
            let handling = target.and_then(|t| auth.handling(&t).ok());
            let fp = fingerprint(&p.envelope.from.key);
            // Checked again for every row shown, against the stored envelope: a
            // post edited in the database after it was accepted shows as not verified.
            let verified = verify_envelope(&p.envelope);
            json!({ "post": p, "handling": handling, "fingerprint": fp, "verified": verified })
        })
        .collect();
    Ok(json!({ "head": auth.head()?, "posts": posts }))
}

// ---- page headers ---------------------------------------------------------------

/// The text of every inline `<script>` in a page.
fn inline_scripts(html: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(open) = rest.find("<script") {
        let Some(start) = rest[open..].find('>').map(|i| open + i + 1) else { break };
        let Some(end) = rest[start..].find("</script>").map(|i| start + i) else { break };
        out.push(&rest[start..end]);
        rest = &rest[end..];
    }
    out
}

/// A Content-Security-Policy for one of our pages. The pages keep a member's
/// token in the browser, so the policy names the exact scripts that may run (by
/// hash), lets the page talk only to the relay it came from, and forbids
/// framing. A script injected through a rendering bug would not run, and could
/// not send the token anywhere if it did.
pub fn content_security_policy(html: &str) -> String {
    let scripts: Vec<String> = inline_scripts(html).iter().map(|s| format!("'sha256-{}'", STANDARD.encode(Sha256::digest(s.as_bytes())))).collect();
    format!(
        "default-src 'none'; script-src {}; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; \
         img-src 'self' data:; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
        if scripts.is_empty() { "'none'".to_string() } else { scripts.join(" ") }
    )
}

/// Headers every HTML page is served with, by the relay and by `backchannel view`.
pub fn page_headers(html: &str) -> Vec<(&'static str, String)> {
    vec![
        ("content-security-policy", content_security_policy(html)),
        ("x-frame-options", "DENY".into()),
        ("referrer-policy", "no-referrer".into()),
        ("x-content-type-options", "nosniff".into()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_policy_names_each_inline_script_by_hash() {
        let page = "<html><head><script>alert(1);</script></head><body><script type=\"module\">\nlet a = 1;\n</script></body></html>";
        assert_eq!(inline_scripts(page), vec!["alert(1);", "\nlet a = 1;\n"]);
        let csp = content_security_policy(page);
        // sha256 of "alert(1);", worked out independently.
        assert!(csp.contains("script-src 'sha256-5jFwrAK0UV47oFbVg/iCCBbxD8X1w+QvoOUepu4C2YA=' 'sha256-"), "{csp}");
        assert!(csp.contains("frame-ancestors 'none'") && csp.contains("connect-src 'self'"));
        assert!(content_security_policy("<p>no scripts</p>").contains("script-src 'none'"));
    }

    #[test]
    fn our_pages_run_no_script_the_policy_does_not_name() {
        for (name, page, scripts) in [
            ("index.html", include_str!("../../../viewer/index.html"), 1),
            ("join.html", include_str!("../../../viewer/join.html"), 1),
            ("home.html", include_str!("../../../viewer/home.html"), 2),
        ] {
            assert_eq!(inline_scripts(page).len(), scripts, "{name}");
            assert_eq!(page.matches("<script").count(), scripts, "{name}: a script the scanner missed");
            let csp = content_security_policy(page);
            let script_src = csp.split(';').find(|d| d.trim().starts_with("script-src")).unwrap();
            assert_eq!(script_src.matches("'sha256-").count(), scripts, "{name}");
            assert!(!script_src.contains("unsafe-inline") && !script_src.contains("unsafe-eval"), "{name}");
            // Hashes only cover <script> blocks: an inline handler would be blocked, so there must be none.
            for handler in [" onclick=", " onload=", " onerror=", " onsubmit=", " oninput=", "javascript:"] {
                assert!(!page.contains(handler), "{name} has an inline {handler} that the policy would block");
            }
            assert!(!page.contains("<script src"), "{name} loads a script from a URL the policy doesn't allow");
        }
    }
}
