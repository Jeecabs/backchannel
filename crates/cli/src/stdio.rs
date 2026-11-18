//! `smoko mcp`: an MCP server over stdio. Works in local mode (tools run here)
//! and remote mode (tools are forwarded to the relay). In local mode it also
//! offers a `smoko://inbox` resource that pushes updates when mail arrives.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use smoko_core::crypto::random_bytes;
use smoko_core::mcp::{self, Incoming, Server};

use crate::backend::{Backend, Local, NOT_SET_UP};
use crate::config;

type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, v: &Value) {
    let mut o = out.lock().unwrap();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

/// "Claude Code" -> "claude-code". Falls back to "agent".
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').chars().take(30).collect::<String>();
    if out.is_empty() { "agent".into() } else { out }
}

pub fn default_label(client: &str) -> String {
    if let Ok(l) = std::env::var("SMOKO_AGENT") {
        if smoko_core::names::is_agent_label(&l) {
            return l;
        }
    }
    let r = random_bytes::<2>();
    format!("{}-{:02x}{:02x}", slug(client), r[0], r[1])
}

fn setup_tool() -> Value {
    smoko_core::tools::tool("setup", "Smoko Break isn't set up on this machine yet. Call this to get instructions for the user.", json!({}), &[])
}

pub fn run() {
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let label = Arc::new(Mutex::new(std::env::var("SMOKO_AGENT").unwrap_or_default()));
    let subscribed = Arc::new(Mutex::new(false));
    let mut client = String::from("agent");
    let local_mode = matches!(config::load().map(|c| c.mode), Some(config::Mode::Local));

    if local_mode {
        spawn_watcher(out.clone(), label.clone(), subscribed.clone());
    }

    let server = Server { name: "smoko", version: env!("CARGO_PKG_VERSION"), instructions: mcp::INSTRUCTIONS, resources: local_mode };
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            send(&out, &mcp::error(Value::Null, -32700, "parse error"));
            continue;
        };
        if msg.get("method").and_then(Value::as_str) == Some("initialize") {
            client = msg["params"]["clientInfo"]["name"].as_str().unwrap_or("agent").to_string();
            let mut l = label.lock().unwrap();
            if l.is_empty() {
                *l = default_label(&client);
            }
        }
        // Re-open per message so `smoko init` takes effect without restarting the agent.
        let backend = Backend::open();
        let tools = match &backend {
            Ok(b) => b.tools().unwrap_or_else(|_| smoko_core::tools::definitions()),
            Err(_) => vec![setup_tool()],
        };
        match server.handle(&msg, &tools) {
            Incoming::Ignore => {}
            Incoming::Respond(v) => send(&out, &v),
            Incoming::Call { id, name, args } => {
                let reply = match &backend {
                    Err(e) => mcp::tool_text(id, &e.message, name != "setup"),
                    Ok(b) => {
                        let mut l = label.lock().unwrap().clone();
                        let r = b.call(&mut l, &client, &name, &args);
                        *label.lock().unwrap() = l;
                        match r {
                            Ok(text) => mcp::tool_text(id, &text, false),
                            Err(e) => mcp::tool_text(id, &format!("{e}"), true),
                        }
                    }
                };
                send(&out, &reply);
            }
            Incoming::Other { id, method, params } => {
                let reply = resource(&method, &params, &label, &subscribed, &client, id.clone());
                send(&out, &reply.unwrap_or_else(|e| mcp::error(id, -32603, &e.message)));
            }
        }
    }
}

fn resource(method: &str, params: &Value, label: &Arc<Mutex<String>>, subscribed: &Arc<Mutex<bool>>, client: &str, id: Value) -> smoko_core::Result<Value> {
    const URI: &str = "smoko://inbox";
    let res = json!({ "uri": URI, "name": "inbox", "title": "Smoko inbox", "description": "Unread Smoko messages for this agent (peek; does not mark them read).", "mimeType": "text/plain" });
    Ok(match method {
        "resources/list" => mcp::result(id, json!({ "resources": [res] })),
        "resources/templates/list" => mcp::result(id, json!({ "resourceTemplates": [] })),
        "resources/subscribe" => {
            *subscribed.lock().unwrap() = params["uri"] == URI;
            mcp::result(id, json!({}))
        }
        "resources/unsubscribe" => {
            *subscribed.lock().unwrap() = false;
            mcp::result(id, json!({}))
        }
        "resources/read" => {
            let text = match Backend::open() {
                Ok(b) => {
                    let mut l = label.lock().unwrap().clone();
                    b.call(&mut l, client, "inbox", &json!({ "peek": true }))?
                }
                Err(_) => NOT_SET_UP.to_string(),
            };
            mcp::result(id, json!({ "contents": [{ "uri": URI, "mimeType": "text/plain", "text": text }] }))
        }
        _ => mcp::error(id, -32601, "method not found"),
    })
}

/// Local mode: watch the shared database and tell the client when this agent has new mail.
fn spawn_watcher(out: Out, label: Arc<Mutex<String>>, subscribed: Arc<Mutex<bool>>) {
    std::thread::spawn(move || {
        let mut last = -1i64;
        loop {
            std::thread::sleep(Duration::from_millis(1000));
            if !*subscribed.lock().unwrap() {
                continue;
            }
            let Some(c) = config::load() else { continue };
            let Ok(node) = Local::open(&c) else { continue };
            let inbox = node.inbox();
            let _ = inbox.fanout();
            let addr = format!("{}/{}", c.name, label.lock().unwrap());
            let unread = inbox.unread(&addr).unwrap_or(0);
            if unread > 0 && unread != last {
                send(&out, &mcp::notification("notifications/resources/updated", json!({ "uri": "smoko://inbox" })));
            }
            last = unread;
        }
    });
}
