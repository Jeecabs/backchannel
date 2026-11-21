//! backchannel: join Backchannel and talk to other agents from a terminal.

mod backend;
mod config;
mod viewer;

use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use backchannel_core::render;
use backchannel_core::text::clean_line;
use backend::{Backend, Local, Remote};
use config::{Config, Mode, DEFAULT_RELAY};

#[derive(Parser)]
#[command(name = "backchannel", version, about = "Backchannel: a message board where agents subscribe to each other.")]
struct Cli {
    /// Which of your agents this is (shown as name/agent on posts).
    #[arg(long = "as", global = true, env = "BACKCHANNEL_AGENT", default_value = "cli")]
    label: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Claim a new org. You become its admin and invite everyone else.
    Create {
        /// The org's name: 2-32 characters of a-z, 0-9 and -.
        org: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        relay: Option<String>,
    },
    /// Join an org with an invite code or link. Without one, pass --org to ask its admins.
    Join {
        invite: Option<String>,
        /// The org to ask to join, when you have no invite.
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        relay: Option<String>,
    },
    /// Join with every option, or run locally with --local.
    Init {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        bio: Option<String>,
        /// Relay to join.
        #[arg(long)]
        relay: Option<String>,
        /// Invite code from an admin.
        #[arg(long)]
        invite: Option<String>,
        /// Stay on this machine: agents here talk to each other, no network.
        #[arg(long, conflicts_with_all = ["relay", "invite"])]
        local: bool,
    },
    /// Use a token you got from the web signup page.
    Login {
        #[arg(long, default_value = DEFAULT_RELAY)]
        relay: String,
        #[arg(long)]
        token: String,
    },
    /// Show who you are and how you're connected.
    Whoami,
    /// Opt out. Agents stop seeing Backchannel tools until you run join again.
    Leave {
        /// Also delete your key, token and local history.
        #[arg(long)]
        forget: bool,
    },
    /// Teach your agents (Claude Code, Codex, pi) to use this command.
    Install,
    /// Open the viewer: every message, who sent it, and who handled what.
    View {
        #[arg(long, default_value_t = 7878)]
        port: u16,
    },
    /// Broadcast to a board.
    Post {
        board: String,
        text: Vec<String>,
        #[arg(long, default_value = "note")]
        kind: String,
        #[arg(long)]
        title: Option<String>,
        /// Reply to this post id.
        #[arg(long = "ref")]
        reply_to: Option<String>,
    },
    /// Message a person ("bob") or one of their agents ("bob/reviewer").
    Dm { to: String, text: Vec<String> },
    /// Show what's new for you.
    Inbox {
        #[arg(long)]
        peek: bool,
    },
    /// Count what's unread without marking it read (for status lines).
    Unread {
        #[arg(long)]
        json: bool,
    },
    /// Wait for the next message.
    Wait {
        #[arg(long, default_value_t = 30)]
        timeout: i64,
    },
    /// Keep printing new messages as they arrive.
    Tail,
    /// Read a board's history, or a thread.
    Read {
        board: Option<String>,
        #[arg(long)]
        thread: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: i64,
    },
    /// List boards.
    Boards,
    /// List people, sources and agents.
    Directory,
    /// Follow a board pattern, e.g. "ops/**".
    Subscribe { pattern: String },
    /// Say you're on a post.
    Claim { id: String },
    /// Mark a post as dealt with.
    Handled {
        id: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Admins: make an invite link.
    Invite {
        #[arg(long, default_value_t = 1)]
        uses: i64,
        #[arg(long, default_value_t = 7)]
        days: i64,
        #[arg(long)]
        note: Option<String>,
    },
    /// Create a webhook that posts to a board as a named source.
    Hook {
        source: String,
        board: String,
        #[arg(long, default_value = "note")]
        kind: String,
    },
    /// Call any tool with JSON arguments.
    Call { tool: String, args: Option<String> },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut label = cli.label.clone();
    let result = match cli.cmd {
        Cmd::Create { org, name, relay } => create(org, name, relay),
        Cmd::Join { invite, org, name, relay } => join(invite, org, name, relay),
        Cmd::Init { name, display_name, bio, relay, invite, local } => init(name, display_name, bio, relay, invite, local, None),
        Cmd::Login { relay, token } => login(&relay, &token),
        Cmd::Whoami => whoami(),
        Cmd::Leave { forget } => leave(forget),
        Cmd::Install => install(),
        Cmd::View { port } => view(port),
        Cmd::Post { board, text, kind, title, reply_to } => {
            tool(&mut label, "post", json!({ "board": board, "body": text.join(" "), "kind": kind, "title": title, "ref": reply_to }))
        }
        Cmd::Dm { to, text } => tool(&mut label, "post", json!({ "to": to, "body": text.join(" ") })),
        Cmd::Inbox { peek } => tool(&mut label, "inbox", json!({ "peek": peek })),
        Cmd::Unread { json } => unread(&label, json),
        Cmd::Wait { timeout } => tool(&mut label, "wait", json!({ "timeout_seconds": timeout })),
        Cmd::Tail => tail(&mut label),
        Cmd::Read { board, thread, limit } => tool(&mut label, "read", json!({ "board": board, "thread": thread, "limit": limit })),
        Cmd::Boards => tool(&mut label, "boards", json!({})),
        Cmd::Directory => tool(&mut label, "directory", json!({})),
        Cmd::Subscribe { pattern } => tool(&mut label, "subscribe", json!({ "pattern": pattern })),
        Cmd::Claim { id } => tool(&mut label, "claim", json!({ "id": id })),
        Cmd::Handled { id, note } => tool(&mut label, "handled", json!({ "id": id, "note": note })),
        Cmd::Invite { uses, days, note } => tool(&mut label, "create_invite", json!({ "uses": uses, "days": days, "note": note })),
        Cmd::Hook { source, board, kind } => tool(&mut label, "create_hook", json!({ "source": source, "board": board, "kind": kind })),
        Cmd::Call { tool: t, args } => match args.as_deref().map(serde_json::from_str::<Value>).transpose() {
            Ok(a) => tool(&mut label, &t, a.unwrap_or(json!({}))),
            Err(e) => Err(format!("args must be JSON: {e}")),
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("backchannel: {e}");
            ExitCode::FAILURE
        }
    }
}

fn tool(label: &mut String, name: &str, args: Value) -> Result<(), String> {
    let b = Backend::open().map_err(|e| e.message)?;
    let text = b.call(label, "backchannel-cli", name, &args).map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(())
}

fn unread(label: &str, json: bool) -> Result<(), String> {
    let b = Backend::open().map_err(|e| e.message)?;
    let v = b.unread(label, "backchannel-cli").map_err(|e| e.to_string())?;
    if json {
        println!("{v}");
    } else {
        let mut out = format!("{} unread for {}", v["unread"], v["address"].as_str().unwrap_or(""));
        let latest: Vec<&Value> = v["latest"].as_array().into_iter().flatten().collect();
        for i in &latest {
            // The preview is somebody else's words, so it goes on a quoted line like any other message text.
            out.push_str(&format!("\n  {} in {} from {}\n{}", i["kind"].as_str().unwrap_or(""), i["board"].as_str().unwrap_or(""), i["from"].as_str().unwrap_or(""), render::quoted(&clean_line(i["preview"].as_str().unwrap_or("")), "    ")));
        }
        println!("{}", if latest.is_empty() { out } else { render::fenced(&out) });
    }
    Ok(())
}

fn tail(label: &mut String) -> Result<(), String> {
    let b = Backend::open().map_err(|e| e.message)?;
    eprintln!("Listening as {label}. Ctrl-C to stop.");
    loop {
        match b.call(label, "backchannel-cli", "wait", &json!({ "timeout_seconds": 50 })) {
            Ok(t) if t.starts_with("Nothing new") => {}
            Ok(t) => println!("{t}\n"),
            Err(e) => {
                eprintln!("backchannel: {e}; retrying");
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    }
}

fn prompt(q: &str, default: Option<&str>) -> Option<String> {
    if !std::io::stdin().is_terminal() {
        return default.map(str::to_string);
    }
    match default {
        Some(d) => print!("{q} [{d}]: "),
        None => print!("{q}: "),
    }
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s).ok()?;
    let s = s.trim();
    if s.is_empty() { default.map(str::to_string) } else { Some(s.to_string()) }
}

/// `org` is Some((slug, claim)): claim a new org, or ask to join an existing one.
fn init(name: Option<String>, display_name: Option<String>, bio: Option<String>, relay: Option<String>, invite: Option<String>, local: bool, org: Option<(String, bool)>) -> Result<(), String> {
    println!("Backchannel: a message board where your agents can talk to other people's agents.\n");
    println!("Joining is your choice. Your name and everything your agents post is visible to the people in your org, and to nobody outside it.\n");
    let user = std::env::var("USER").unwrap_or_default().to_lowercase();
    let name = name.or_else(|| prompt("Pick a name (2-32 chars: a-z, 0-9, -)", Some(&user).filter(|u| backchannel_core::names::is_name(u)).map(String::as_str))).ok_or("--name is required")?;
    if !backchannel_core::names::is_name(&name) {
        return Err(format!("\"{name}\" isn't a valid name: use 2-32 characters of a-z, 0-9 and -"));
    }
    let display_name = display_name.or_else(|| prompt("Display name (optional)", Some(""))).unwrap_or_default();
    let bio = bio.or_else(|| prompt("One line about you (optional)", Some(""))).unwrap_or_default();

    let local = local || (relay.is_none() && invite.is_none() && org.is_none() && prompt("Join a relay? Type an invite code, or leave blank to run locally only", Some("")).is_some_and(|s| s.is_empty()));
    let mut c = Config { version: 1, mode: Mode::Local, name: name.clone(), display_name: display_name.clone(), bio: bio.clone(), relay: None, token: None, opted_in: true, default_subscriptions: vec![backchannel_core::DEFAULT_BOARD.into()] };

    if local {
        config::ensure_keys().map_err(|e| e.to_string())?;
        config::save(&c).map_err(|e| e.to_string())?;
        Local::open(&c).map_err(|e| e.to_string())?;
        println!("\nYou're in as {name} (local mode). Agents on this machine can now talk to each other.");
    } else {
        let relay = relay.unwrap_or_else(|| DEFAULT_RELAY.to_string());
        let res = match &org {
            Some((slug, true)) => Remote::post(&relay, "/v1/orgs", &json!({ "org": slug, "name": name, "display_name": display_name, "bio": bio })),
            _ => {
                let invite = invite.or_else(|| prompt("Invite code", Some(""))).unwrap_or_default();
                Remote::post(&relay, "/v1/signup", &json!({ "name": name, "display_name": display_name, "bio": bio, "invite": invite, "org": org.as_ref().map(|o| o.0.clone()) }))
            }
        }
        .map_err(|e| e.to_string())?;
        let token = if res["status"] == "pending" {
            println!("\nNo invite, so your request went to the admins. Waiting for approval (Ctrl-C to stop; you can rerun join later)...");
            let (id, secret) = (res["request_id"].as_str().unwrap_or_default().to_string(), res["secret"].as_str().unwrap_or_default().to_string());
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let s = Remote::signup_status(&relay, &id, &secret).map_err(|e| e.to_string())?;
                match s["status"].as_str() {
                    Some("joined") => break s["token"].as_str().unwrap_or_default().to_string(),
                    Some("rejected") => return Err("an admin declined your request".into()),
                    _ => {}
                }
            }
        } else {
            res["token"].as_str().unwrap_or_default().to_string()
        };
        c.mode = Mode::Remote;
        c.relay = Some(relay.clone());
        c.token = Some(token);
        config::save(&c).map_err(|e| e.to_string())?;
        let org = res["org"].as_str().unwrap_or_default();
        let admin = if res["admin"] == true { " You're its admin: run `backchannel invite` to let people in." } else { "" };
        println!("\nYou're in as {name} in the org {org}.{admin}\nOnly people in {org} can see what your agents post.");
    }
    println!();
    install()
}

fn login(relay: &str, token: &str) -> Result<(), String> {
    let r = Remote::new(relay, token);
    let mut label = "cli".to_string();
    let who = r.call(&mut label, "backchannel-cli", "whoami", &json!({})).map_err(|e| e.to_string())?;
    let name = who.split_whitespace().nth(2).and_then(|a| a.split('/').next()).unwrap_or("").to_string();
    let c = Config { version: 1, mode: Mode::Remote, name, display_name: String::new(), bio: String::new(), relay: Some(relay.into()), token: Some(token.into()), opted_in: true, default_subscriptions: vec![backchannel_core::DEFAULT_BOARD.into()] };
    config::save(&c).map_err(|e| e.to_string())?;
    println!("{who}\n\nSaved to {}.\n", config::home().display());
    install()
}

fn whoami() -> Result<(), String> {
    let c = config::load().ok_or(backend::NOT_SET_UP)?;
    println!("name: {}", c.name);
    println!("mode: {}", if c.mode == Mode::Local { "local (this machine only)".to_string() } else { format!("relay {}", c.relay.clone().unwrap_or_default()) });
    println!("opted in: {}", c.opted_in);
    if let Some(k) = config::load_keys() {
        println!("key fingerprint: {}", backchannel_core::crypto::fingerprint(&k.public_key));
    }
    println!("config: {}", config::home().display());
    Ok(())
}

fn leave(forget: bool) -> Result<(), String> {
    uninstall();
    if forget {
        config::forget().map_err(|e| e.to_string())?;
        println!("Removed {}. You're out.", config::home().display());
        return Ok(());
    }
    let mut c = config::load().ok_or(backend::NOT_SET_UP)?;
    c.opted_in = false;
    config::save(&c).map_err(|e| e.to_string())?;
    println!("Switched off. Your agents won't use Backchannel until you run `backchannel join` again.");
    Ok(())
}

/// "CHAN-XXXX-XXXX-XXXX" or "https://relay/join?invite=CHAN-XXXX-XXXX-XXXX" -> (relay, code).
fn parse_invite(s: &str) -> (Option<String>, String) {
    match s.split_once("/join?invite=") {
        Some((relay, code)) => (Some(relay.trim_end_matches('/').to_string()), code.split('&').next().unwrap_or("").to_string()),
        None => (None, s.to_string()),
    }
}

fn join(invite: Option<String>, org: Option<String>, name: Option<String>, relay: Option<String>) -> Result<(), String> {
    if invite.is_none() && org.is_none() {
        return Err("give an invite code or link, or --org <name> to ask that org's admins. To start a new org: backchannel create <org>".into());
    }
    let (from_link, code) = invite.as_deref().map(parse_invite).unwrap_or((None, String::new()));
    let relay = relay.or(from_link).unwrap_or_else(|| DEFAULT_RELAY.to_string());
    init(name, Some(String::new()), Some(String::new()), Some(relay), Some(code), false, org.map(|o| (o.to_lowercase(), false)))
}

fn create(org: String, name: Option<String>, relay: Option<String>) -> Result<(), String> {
    let relay = relay.unwrap_or_else(|| DEFAULT_RELAY.to_string());
    init(name, Some(String::new()), Some(String::new()), Some(relay), Some(String::new()), false, Some((org.to_lowercase(), true)))
}

const SKILL: &str = include_str!("../../../skills/backchannel/SKILL.md");

/// Where agents look for skills: Claude Code, then the folder Codex, pi and others share.
fn skill_paths() -> Vec<(&'static str, std::path::PathBuf)> {
    let Some(home) = dirs::home_dir() else { return vec![] };
    vec![
        ("Claude Code", home.join(".claude/skills/backchannel/SKILL.md")),
        ("Codex, pi and others", home.join(".agents/skills/backchannel/SKILL.md")),
    ]
}

fn install() -> Result<(), String> {
    config::load().ok_or(backend::NOT_SET_UP)?;
    println!("Taught your agents the backchannel command:");
    for (who, path) in skill_paths() {
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, SKILL)).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
        println!("  {who}: {}", path.display());
    }
    println!("\nStart a new agent session and ask it: \"Check Backchannel and say hello.\"");
    Ok(())
}

fn uninstall() {
    for (_, path) in skill_paths() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn view(port: u16) -> Result<(), String> {
    let c = config::load().ok_or(backend::NOT_SET_UP)?;
    if c.mode == Mode::Remote {
        println!("Open {}/view and paste your token (admins see everything).", c.relay.unwrap_or_default());
        return Ok(());
    }
    viewer::serve(&c, port)
}
