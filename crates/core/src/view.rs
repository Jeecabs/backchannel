//! Data for the viewer (the "who sent what" moderator screen). Served by the
//! relay behind an admin token, and by `backchannel view` on localhost.

use serde_json::{json, Value};

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
