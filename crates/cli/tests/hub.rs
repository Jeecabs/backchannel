//! `backchannel hub` end to end: the real binary, speaking MCP on its stdin
//! and stdout, against a local node in a temporary home. Two people share the
//! node's database, the way two terminals on one machine do.
//!
//! Nothing here touches the real ~/.backchannel, the real skill folders or a
//! relay: every command runs with its own $BACKCHANNEL_HOME and $HOME, and
//! $BACKCHANNEL_RELAY points at a port nothing listens on.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_backchannel");
const CHANGED: &str = "notifications/t3/integration/changed";

fn command(home: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.env("HOME", home)
        .env("BACKCHANNEL_HOME", home.join(".backchannel"))
        .env("BACKCHANNEL_RELAY", "http://127.0.0.1:9")
        .env("BACKCHANNEL_HUB_POLL_MS", "200")
        .env_remove("BACKCHANNEL_AGENT")
        .env_remove("BACKCHANNEL_HUB_LABEL")
        .env_remove("BACKCHANNEL_TOKEN");
    c
}

/// Runs one command to its end and returns what it printed.
fn run(home: &Path, args: &[&str]) -> String {
    let out = command(home)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The id in "Posted ask to smoko (id 01JD..., seq 3)."
fn posted(text: &str) -> String {
    let id = text.split("(id ").nth(1).expect(text);
    id.split(',').next().unwrap().to_string()
}

struct Hub {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Value>,
    next: u64,
    /// Notifications that arrived while waiting for an answer.
    notices: Vec<Value>,
}

impl Hub {
    fn start(home: &Path) -> Hub {
        let mut child = command(home)
            .arg("hub")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                // Every line on stdout must be a JSON-RPC message.
                let msg: Value = serde_json::from_str(&line).expect(&line);
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        let mut hub = Hub {
            child,
            stdin,
            lines,
            next: 0,
            notices: vec![],
        };
        let hello = hub.request(
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }),
        );
        assert_eq!(hello["serverInfo"]["name"], "backchannel");
        assert_eq!(hello["protocolVersion"], "2025-06-18");
        hub.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        hub
    }

    fn send(&mut self, msg: Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let msg = self
                .lines
                .recv_timeout(Duration::from_secs(30))
                .expect("the hub answered");
            if msg["id"] == json!(id) {
                assert!(msg.get("error").is_none(), "{method}: {msg}");
                return msg["result"].clone();
            }
            self.notices.push(msg);
        }
    }

    /// A tool's whole answer, error or not.
    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.request("tools/call", json!({ "name": name, "arguments": args }))
    }

    /// What a tool answered in `structuredContent`; fails if it refused.
    fn ok(&mut self, name: &str, args: Value) -> Value {
        let answer = self.tool(name, args);
        assert_eq!(answer["isError"], false, "{name}: {answer}");
        answer["structuredContent"].clone()
    }

    /// What a tool said when it refused.
    fn refused(&mut self, name: &str, args: Value) -> String {
        let answer = self.tool(name, args);
        assert_eq!(answer["isError"], true, "{name}: {answer}");
        answer["content"][0]["text"].as_str().unwrap().to_string()
    }

    /// Waits for the next change notice, and forgets the ones before it.
    fn changed(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if self.notices.iter().any(|n| n["method"] == CHANGED) {
                self.notices.clear();
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = self.lines.recv_timeout(left).expect("a change notice");
            self.notices.push(msg);
        }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("backchannel-hub-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn tool_meta<'a>(tools: &'a Value, name: &str) -> &'a Value {
    let tool = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("no tool {name}"));
    &tool["_meta"]["t3/integration"]
}

fn refs(view: &Value) -> Vec<String> {
    view["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["ref"].as_str().unwrap().to_string())
        .collect()
}

fn actions(detail: &Value) -> Vec<&str> {
    detail["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect()
}

#[cfg(unix)]
#[test]
fn the_page_reads_and_acts_on_a_shared_node() {
    let root = temp("node");
    let (alice, bob) = (root.join("alice"), root.join("bob"));
    std::fs::create_dir_all(&alice).unwrap();
    run(&alice, &["init", "--local", "--name", "alice"]);
    // Bob is another person on the same node: his own key, the same database.
    std::fs::create_dir_all(bob.join(".backchannel")).unwrap();
    std::os::unix::fs::symlink(
        alice.join(".backchannel/data"),
        bob.join(".backchannel/data"),
    )
    .unwrap();
    run(&bob, &["init", "--local", "--name", "bob"]);

    let mut hub = Hub::start(&alice);

    // The tool list says what each tool is for.
    let tools = hub.request("tools/list", json!({}));
    assert_eq!(tool_meta(&tools, "status")["role"], "status");
    for (name, title) in [
        ("inbox", "For you"),
        ("jobs", "Open jobs"),
        ("boards", "Boards"),
        ("people", "People and agents"),
    ] {
        let meta = tool_meta(&tools, name);
        assert_eq!(
            (&meta["role"], &meta["title"]),
            (&json!("view"), &json!(title))
        );
        assert_eq!(meta["inbox"] == true, name == "inbox");
    }
    assert_eq!(tool_meta(&tools, "item")["role"], "item");
    for (name, kind, scope) in [
        ("reply", "reply", "operate"),
        ("post", "reply", "operate"),
        ("message", "reply", "operate"),
        ("claim", "button", "operate"),
        ("release", "button", "operate"),
        ("handled", "button", "operate"),
        ("start", "starter", "operate"),
        ("mark_read", "acknowledge", "operate"),
        ("setup", "setup", "manage"),
        ("remove", "remove", "manage"),
    ] {
        let meta = tool_meta(&tools, name);
        assert_eq!(meta["role"], "action", "{name}");
        assert_eq!(
            (&meta["kind"], &meta["scope"]),
            (&json!(kind), &json!(scope)),
            "{name}"
        );
    }
    for name in ["claim", "release", "handled"] {
        assert_eq!(tool_meta(&tools, name)["onItem"], true);
    }
    assert_eq!(tool_meta(&tools, "mark_read")["view"], "inbox");
    // The host puts an item's title after these, so each reads as a phrase.
    for (name, title) in [
        ("reply", "Reply to"),
        ("post", "Post to"),
        ("message", "Message"),
    ] {
        assert_eq!(tool_meta(&tools, name)["title"], title);
    }
    assert!(tool_meta(&tools, "remove")["confirm"].is_string());
    let kind = &tool_meta(&tools, "post")["fields"][0];
    assert_eq!(
        (&kind["name"], &kind["type"]),
        (&json!("kind"), &json!("choice"))
    );
    assert_eq!(kind["options"].as_array().unwrap().len(), 3);
    let note = &tool_meta(&tools, "handled")["fields"][0];
    assert_eq!(
        (&note["name"], &note["type"]),
        (&json!("note"), &json!("text"))
    );
    assert!(note.get("required").is_none());
    // Already joined, so setup asks for nothing.
    assert!(tool_meta(&tools, "setup").get("fields").is_none());

    let status = hub.ok("status", json!({}));
    assert_eq!(status["state"], "ready");
    assert!(status["message"].as_str().unwrap().contains("alice/desk"));
    assert_eq!(status["steps"][0]["state"], "done");
    assert_eq!(status["steps"][1]["state"], "done");
    assert!(refs(&hub.ok("jobs", json!({}))).is_empty());
    hub.notices.clear();

    // Someone else posts an ask: the page is told, and shows it.
    let ask = posted(&run(
        &bob,
        &[
            "--as",
            "reviewer",
            "post",
            "smoko",
            "Can someone review PR 7?",
            "--kind",
            "ask",
        ],
    ));
    let job = format!("post:{ask}");
    hub.changed();
    let jobs = hub.ok("jobs", json!({}));
    assert_eq!(refs(&jobs), std::slice::from_ref(&job));
    let first = &jobs["items"][0];
    assert_eq!(first["title"], "Can someone review PR 7?");
    assert_eq!(first["group"], "smoko");
    assert_eq!(
        first["status"],
        json!({ "label": "Open", "tone": "attention" })
    );
    assert_eq!(first["facts"][0]["text"], "bob/reviewer");
    let inbox = hub.ok("inbox", json!({}));
    assert_eq!(refs(&inbox), std::slice::from_ref(&job));
    assert_eq!(inbox["items"][0]["title"], "bob/reviewer");
    assert_eq!(inbox["items"][0]["body"], "Can someone review PR 7?");
    assert!(inbox["items"][0]["note"]
        .as_str()
        .unwrap()
        .contains("smoko"));

    // Open: anyone may claim it, start on it, reply or mark it handled.
    let open = hub.ok("item", json!({ "ref": job }));
    assert_eq!(actions(&open), ["claim", "start", "reply", "handled"]);
    assert_eq!(open["timeline"][0]["author"], "bob/reviewer");
    assert_eq!(open["timeline"][0]["text"], "Can someone review PR 7?");

    // Claimed by the page.
    assert_eq!(
        hub.ok("claim", json!({ "ref": job }))["message"],
        "Claimed for 10 minutes"
    );
    let claimed = hub.ok("item", json!({ "ref": job }));
    assert_eq!(actions(&claimed), ["release", "handled", "start", "reply"]);
    assert_eq!(claimed["item"]["status"]["label"], "Claimed by you");
    let lease = claimed["item"]["facts"].as_array().unwrap().last().unwrap();
    assert_eq!(lease["icon"], "clock");
    let until = lease["text"].as_str().unwrap();
    assert!(
        until.starts_with("until ") && until.ends_with(" UTC"),
        "{until}"
    );
    assert_eq!(claimed["item"]["status"]["tone"], "active");
    assert_eq!(claimed["live"], true);
    assert_eq!(claimed["timeline"][1]["kind"], "event");
    assert_eq!(claimed["timeline"][1]["label"], "Claimed");

    // A reply reads as the person's own.
    hub.ok("reply", json!({ "ref": job, "text": "Looking now." }));
    let replied = hub.ok("item", json!({ "ref": job }));
    let last = replied["timeline"].as_array().unwrap().last().unwrap();
    assert_eq!(
        (&last["side"], &last["text"]),
        (&json!("end"), &json!("Looking now."))
    );

    // Starting a thread on it renews the claim and quotes the job as untrusted.
    let prompt = hub.ok("start", json!({ "ref": job }))["prompt"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(prompt.contains("SECURITY NOTE"), "{prompt}");
    assert!(prompt.contains("for the next 30 minutes"));
    assert!(prompt.contains("  | Can someone review PR 7?"));
    assert!(prompt.contains("END OF BACKCHANNEL MESSAGES"));
    assert!(prompt.contains(&format!("backchannel --as desk handled {ask} --note")));
    assert!(prompt.contains(&format!("backchannel --as desk release {ask}")));
    // Nothing the asker wrote sits outside the quoted lines.
    for line in prompt.lines().filter(|l| l.contains("PR 7")) {
        assert!(line.trim_start().starts_with("| "), "{line}");
    }

    // Released and claimed again, then handled with a note.
    hub.ok("release", json!({ "ref": job }));
    assert_eq!(
        actions(&hub.ok("item", json!({ "ref": job }))),
        ["claim", "start", "reply", "handled"]
    );
    hub.ok("claim", json!({ "ref": job }));
    hub.ok("handled", json!({ "ref": job, "note": "Approved." }));
    let handled = hub.ok("item", json!({ "ref": job }));
    assert_eq!(actions(&handled), ["reply"]);
    assert_eq!(handled["item"]["status"]["label"], "Handled by you");
    let last = handled["timeline"].as_array().unwrap().last().unwrap();
    assert_eq!(
        (&last["label"], &last["text"]),
        (&json!("Handled"), &json!("alice/desk: Approved."))
    );
    assert!(refs(&hub.ok("jobs", json!({}))).is_empty());
    assert!(hub
        .refused("start", json!({ "ref": job }))
        .contains("already handled"));

    // A job someone else holds can only be replied to, and can't be started.
    let task = posted(&run(
        &bob,
        &[
            "--as",
            "reviewer",
            "post",
            "smoko",
            "Rotate the staging key",
            "--kind",
            "task",
        ],
    ));
    run(&bob, &["--as", "reviewer", "claim", &task]);
    let theirs = hub.ok("item", json!({ "ref": format!("post:{task}") }));
    assert_eq!(actions(&theirs), ["reply"]);
    assert_eq!(theirs["item"]["status"]["label"], "Claimed by bob/reviewer");
    assert_eq!(theirs["item"]["note"], "task");
    assert!(hub
        .refused("claim", json!({ "ref": format!("post:{task}") }))
        .contains("bob/reviewer"));
    assert!(hub
        .refused("start", json!({ "ref": format!("post:{task}") }))
        .contains("bob/reviewer is already on this"));

    // Starting on an open job claims it.
    let fresh = posted(&run(
        &bob,
        &[
            "--as",
            "reviewer",
            "post",
            "smoko",
            "Update the changelog",
            "--kind",
            "task",
        ],
    ));
    assert!(hub.ok("start", json!({ "ref": format!("post:{fresh}") }))["prompt"].is_string());
    let started = hub.ok("item", json!({ "ref": format!("post:{fresh}") }));
    assert_eq!(started["item"]["status"]["label"], "Claimed by you");

    // Boards, and posting to one with a kind.
    assert!(refs(&hub.ok("boards", json!({}))).contains(&"board:smoko".to_string()));
    let board = hub.ok("item", json!({ "ref": "board:smoko" }));
    assert_eq!(actions(&board), ["post"]);
    assert_eq!(
        hub.ok(
            "post",
            json!({ "ref": "board:smoko", "text": "Who broke staging?", "kind": "ask" })
        )["message"],
        "Posted to smoko"
    );
    let jobs = hub.ok("jobs", json!({}));
    assert!(jobs["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["title"] == "Who broke staging?" && i["note"] == "ask"));
    assert!(hub
        .refused(
            "post",
            json!({ "ref": "board:smoko", "text": "x", "kind": "claim" })
        )
        .contains("note, an ask or a task"));
    assert!(hub
        .refused("post", json!({ "ref": "board:smoko", "text": " " }))
        .contains("nothing to send"));

    // People and agents, and direct messages both ways.
    let people = refs(&hub.ok("people", json!({})));
    for expected in [
        "person:alice",
        "person:bob",
        "agent:bob/reviewer",
        "agent:alice/desk",
    ] {
        assert!(
            people.contains(&expected.to_string()),
            "{expected} in {people:?}"
        );
    }
    assert_eq!(
        hub.ok(
            "message",
            json!({ "ref": "agent:bob/reviewer", "text": "One nit on the cookie." })
        )["message"],
        "Sent to bob/reviewer"
    );
    assert!(run(&bob, &["--as", "reviewer", "inbox"]).contains("| One nit on the cookie."));
    run(&bob, &["--as", "reviewer", "dm", "alice", "Fixed, thanks."]);
    let direct = hub.ok("item", json!({ "ref": "person:bob" }));
    assert_eq!(actions(&direct), ["message"]);
    let said: Vec<(&Value, &Value)> = direct["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (&e["text"], &e["author"]))
        .collect();
    assert!(
        said.contains(&(&json!("Fixed, thanks."), &json!("bob/reviewer"))),
        "{said:?}"
    );
    let with_agent = hub.ok("item", json!({ "ref": "agent:bob/reviewer" }));
    assert!(with_agent["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["side"] == "end" && e["text"] == "One nit on the cookie."));

    // The inbox holds what arrived, and marking it read empties it.
    let inbox = hub.ok("inbox", json!({}));
    let waiting = inbox["items"].as_array().unwrap();
    assert!(waiting
        .iter()
        .any(|i| i["body"] == "Fixed, thanks."
            && i["note"].as_str().unwrap().contains("direct message")));
    // What happened to the person's own post opens that post.
    let mine = jobs["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["title"] == "Who broke staging?")
        .unwrap()["ref"]
        .as_str()
        .unwrap()
        .to_string();
    run(
        &bob,
        &[
            "--as",
            "reviewer",
            "claim",
            mine.trim_start_matches("post:"),
        ],
    );
    let inbox = hub.ok("inbox", json!({}));
    let claim = inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["opens"] == json!(mine))
        .expect("the claim is in the inbox");
    assert_eq!(claim["title"], "bob/reviewer claimed a post");
    assert!(claim["note"]
        .as_str()
        .unwrap()
        .contains("claim on your ask"));
    let opened = hub.ok("item", json!({ "ref": claim["ref"] }));
    assert_eq!(opened["item"]["ref"], json!(mine));
    assert_eq!(hub.ok("mark_read", json!({}))["message"], "Marked as read");
    assert!(refs(&hub.ok("inbox", json!({}))).is_empty());

    assert!(hub
        .refused("item", json!({ "ref": "nonsense" }))
        .contains("can open"));

    // Setup rewrites the skill files; removing switches off; setup switches back on.
    let skill = alice.join(".claude/skills/backchannel/SKILL.md");
    std::fs::remove_file(&skill).unwrap();
    assert_eq!(hub.ok("status", json!({}))["state"], "setup");
    assert_eq!(hub.ok("setup", json!({}))["message"], "Skill files written");
    assert!(skill.exists());
    hub.ok("remove", json!({}));
    let off = hub.ok("status", json!({}));
    assert_eq!(
        (&off["state"], &off["steps"][0]["note"]),
        (&json!("setup"), &json!("Switched off"))
    );
    let tools = hub.request("tools/list", json!({}));
    assert_eq!(tool_meta(&tools, "setup")["title"], "Switch back on");
    assert!(hub.refused("jobs", json!({})).contains("switched off"));
    hub.ok("setup", json!({}));
    assert_eq!(hub.ok("status", json!({}))["state"], "ready");

    drop(hub);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_machine_that_has_not_joined_is_asked_for_an_invite_and_a_name() {
    let home = temp("new");
    let mut hub = Hub::start(&home);
    let status = hub.ok("status", json!({}));
    assert_eq!(status["state"], "setup");
    assert_eq!(status["steps"][0]["state"], "todo");
    let tools = hub.request("tools/list", json!({}));
    let setup = tool_meta(&tools, "setup");
    assert_eq!(setup["title"], "Join");
    let fields: Vec<(&Value, &Value)> = setup["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (&f["name"], &f["required"]))
        .collect();
    assert_eq!(
        fields,
        [
            (&json!("invite"), &json!(true)),
            (&json!("name"), &json!(true))
        ]
    );
    // Refused before anything is sent anywhere.
    assert!(hub
        .refused("setup", json!({ "name": "alice" }))
        .contains("invite"));
    assert!(hub
        .refused(
            "setup",
            json!({ "invite": "CHAN-AAAA-BBBB-CCCC", "name": "A!" })
        )
        .contains("2 to 32"));
    assert!(hub.refused("jobs", json!({})).contains("not set up"));
    assert!(!home.join(".backchannel").exists());
    drop(hub);
    std::fs::remove_dir_all(&home).unwrap();
}
