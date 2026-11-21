//! `backchannel view`: the viewer on localhost, reading this machine's database.

use backchannel_core::view;
use serde_json::json;
use tiny_http::{Header, Response, Server};

use crate::backend::Local;
use crate::config::Config;

pub const HTML: &str = include_str!("../../../viewer/index.html");

pub fn serve(c: &Config, port: u16) -> Result<(), String> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    println!(
        "Backchannel viewer on http://127.0.0.1:{port} (local mode, read-only). Ctrl-C to stop."
    );
    // Only answer requests addressed to this machine by name. A web page that
    // points its own hostname at 127.0.0.1 would otherwise be able to read the feed.
    let hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    for req in server.incoming_requests() {
        let host = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("host"))
            .map(|h| h.value.as_str().to_string())
            .unwrap_or_default();
        if !hosts.contains(&host) {
            let _ = req.respond(Response::from_string("not found").with_status_code(404));
            continue;
        }
        let url = req.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((&url, ""));
        let json_header = Header::from_bytes("content-type", "application/json").unwrap();
        let resp = match path {
            "/" | "/view" => {
                let mut page = Response::from_string(HTML).with_header(
                    Header::from_bytes("content-type", "text/html; charset=utf-8").unwrap(),
                );
                for (name, value) in view::page_headers(HTML) {
                    page.add_header(Header::from_bytes(name, value).unwrap());
                }
                page
            }
            "/v1/view/state" | "/v1/view/feed" => {
                let body = Local::open(c).and_then(|node| {
                    let inbox = node.inbox();
                    if path.ends_with("state") {
                        view::state(
                            &node.auth,
                            &inbox,
                            &format!("{} (local)", c.name),
                            json!({ "mode": "local" }),
                        )
                    } else {
                        let after = query
                            .split('&')
                            .find_map(|kv| kv.strip_prefix("after="))
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        view::feed(&node.auth, after, 500)
                    }
                });
                match body {
                    Ok(v) => Response::from_string(v.to_string()).with_header(json_header),
                    Err(e) => Response::from_string(e.to_json().to_string())
                        .with_status_code(500)
                        .with_header(json_header),
                }
            }
            _ => Response::from_string("not found").with_status_code(404),
        };
        let _ = req.respond(resp);
    }
    Ok(())
}
