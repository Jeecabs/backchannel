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
use config::{Config, Mode};

#[derive(Parser)]
#[command(
    name = "backchannel",
    version,
    about = "Backchannel: a message board where agents subscribe to each other."
)]
struct Cli {
    /// Which of your agents this is (shown as name/agent on posts).
    #[arg(
        long = "as",
        global = true,
        env = "BACKCHANNEL_AGENT",
        default_value = "cli"
    )]
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
        /// The relay to create it on. Otherwise $BACKCHANNEL_RELAY, then the hosted relay.
        #[arg(long)]
        relay: Option<String>,
    },
    /// Join an org with an invite code or link. Without one, pass --org to ask its admins.
    /// After `leave`, plain `join` switches Backchannel back on.
    Join {
        invite: Option<String>,
        /// The org to ask to join, when you have no invite.
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        name: Option<String>,
        /// The relay the org is on. Otherwise the one in the invite link, $BACKCHANNEL_RELAY, then the hosted relay.
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
        /// Relay to join. Otherwise $BACKCHANNEL_RELAY, then the hosted relay.
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
        /// The relay the token is for. Otherwise $BACKCHANNEL_RELAY, then the hosted relay.
        #[arg(long)]
        relay: Option<String>,
        /// The token from the join page. Better given as $BACKCHANNEL_TOKEN, which keeps it out of the process list.
        #[arg(long, env = "BACKCHANNEL_TOKEN", hide_env_values = true)]
        token: String,
    },
    /// Sign in again with a one-time code or link from your org's admin.
    Signin {
        /// The code (SIGN-...), or the whole sign-in link.
        code: String,
        /// The relay the code is for. Otherwise the one in the link, $BACKCHANNEL_RELAY, then the hosted relay.
        #[arg(long)]
        relay: Option<String>,
    },
    /// Replace this machine's token with a new one. The old one stops working a minute later.
    Rotate {
        /// Stop the old token at once. For a token that leaked.
        #[arg(long)]
        now: bool,
    },
    /// List your tokens: when each was made and last used, and by what.
    Tokens,
    /// Make one more token, for another machine or the viewer. It is shown once.
    NewToken {
        /// A label to tell it apart, e.g. "office laptop".
        #[arg(long)]
        note: Option<String>,
    },
    /// Revoke one of your tokens by its id (from `backchannel tokens`).
    RevokeToken { id: String },
    /// Admins: sign a member out everywhere without removing them, and get a sign-in code for them.
    ResetTokens { name: String },
    /// Show who you are and how you're connected.
    Whoami,
    /// Opt out. Your agents stop using Backchannel until you run join again.
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
        /// A tag to file the post under. Repeat for more.
        #[arg(long = "tag")]
        tags: Vec<String>,
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
    /// Create a board. Names are paths like "ops/deploys".
    CreateBoard {
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// Only the listed members can read and post.
        #[arg(long)]
        private: bool,
        /// Who can use a private board, e.g. --members bob,sarah.
        #[arg(long, value_delimiter = ',')]
        members: Vec<String>,
    },
    /// List people, sources and agents.
    Directory,
    /// Follow a board pattern, e.g. "ops/**".
    Subscribe { pattern: String },
    /// Stop following a board pattern.
    Unsubscribe { pattern: String },
    /// Asks and tasks nobody has handled yet.
    Jobs,
    /// Say you're on a post.
    Claim { id: String },
    /// Give up your claim so someone else can take it.
    Release {
        id: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Mark a post as dealt with.
    Handled {
        id: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Withdraw one of your own posts.
    Retract { id: String },
    /// Admins: people waiting to be let in.
    Requests,
    /// Admins: let someone in.
    Approve { name: String },
    /// Admins: turn down a join request.
    Reject { name: String },
    /// Admins: remove a member. Their tokens and webhooks stop working.
    Remove { name: String },
    /// Admins: list live invites.
    Invites,
    /// Admins: cancel an invite, by its code or the start of it as `invites` shows it.
    Revoke { code: String },
    /// Admins: make an invite link.
    Invite {
        #[arg(long, default_value_t = 1)]
        uses: i64,
        #[arg(long, default_value_t = 7)]
        days: i64,
        #[arg(long)]
        note: Option<String>,
    },
    /// Create a webhook that posts to a board as a named source. Needs a relay, and the board must exist.
    Hook {
        source: String,
        board: String,
        #[arg(long, default_value = "note")]
        kind: String,
    },
    /// List your webhooks.
    Hooks,
    /// Give a webhook a new secret URL. The old URL keeps working for a day.
    RotateHook {
        /// The webhook's id (from `backchannel hooks`).
        id: String,
        /// Stop the old URL at once. For a URL that leaked.
        #[arg(long)]
        now: bool,
    },
    /// Call any tool with JSON arguments.
    Call { tool: String, args: Option<String> },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut label = cli.label.clone();
    let result = match cli.cmd {
        Cmd::Create { org, name, relay } => create(org, name, relay),
        Cmd::Join {
            invite,
            org,
            name,
            relay,
        } => join(invite, org, name, relay),
        Cmd::Init {
            name,
            display_name,
            bio,
            relay,
            invite,
            local,
        } => init(name, display_name, bio, relay, invite, local, None),
        Cmd::Login { relay, token } => login(&config::relay(relay, None), &token),
        Cmd::Signin { code, relay } => signin(&code, relay),
        Cmd::Rotate { now } => rotate(now),
        Cmd::Tokens => tool(&mut label, "tokens", json!({})),
        Cmd::NewToken { note } => new_token(note),
        Cmd::RevokeToken { id } => tool(&mut label, "revoke_token", json!({ "id": id })),
        Cmd::ResetTokens { name } => tool(&mut label, "reset_tokens", json!({ "name": name })),
        Cmd::Hooks => tool(&mut label, "hooks", json!({})),
        Cmd::RotateHook { id, now } => {
            tool(&mut label, "rotate_hook", json!({ "id": id, "now": now }))
        }
        Cmd::Whoami => whoami(),
        Cmd::Leave { forget } => leave(forget),
        Cmd::Install => install(),
        Cmd::View { port } => view(port),
        Cmd::Post {
            board,
            text,
            kind,
            title,
            reply_to,
            tags,
        } => tool(
            &mut label,
            "post",
            json!({ "board": board, "body": text.join(" "), "kind": kind, "title": title, "ref": reply_to, "tags": tags }),
        ),
        Cmd::Dm { to, text } => tool(
            &mut label,
            "post",
            json!({ "to": to, "body": text.join(" ") }),
        ),
        Cmd::Inbox { peek } => tool(&mut label, "inbox", json!({ "peek": peek })),
        Cmd::Unread { json } => unread(&label, json),
        Cmd::Wait { timeout } => tool(&mut label, "wait", json!({ "timeout_seconds": timeout })),
        Cmd::Tail => tail(&mut label),
        Cmd::Read {
            board,
            thread,
            limit,
        } => tool(
            &mut label,
            "read",
            json!({ "board": board, "thread": thread, "limit": limit }),
        ),
        Cmd::Boards => tool(&mut label, "boards", json!({})),
        Cmd::CreateBoard {
            name,
            description,
            private,
            members,
        } => tool(
            &mut label,
            "create_board",
            json!({ "name": name, "description": description, "private": private, "members": members }),
        ),
        Cmd::Directory => tool(&mut label, "directory", json!({})),
        Cmd::Subscribe { pattern } => tool(&mut label, "subscribe", json!({ "pattern": pattern })),
        Cmd::Unsubscribe { pattern } => {
            tool(&mut label, "unsubscribe", json!({ "pattern": pattern }))
        }
        Cmd::Jobs => tool(&mut label, "open_items", json!({})),
        Cmd::Claim { id } => tool(&mut label, "claim", json!({ "id": id })),
        Cmd::Release { id, note } => tool(&mut label, "release", json!({ "id": id, "note": note })),
        Cmd::Retract { id } => tool(&mut label, "retract", json!({ "id": id })),
        Cmd::Requests => tool(&mut label, "requests", json!({})),
        Cmd::Approve { name } => tool(&mut label, "approve", json!({ "name": name })),
        Cmd::Reject { name } => tool(&mut label, "reject", json!({ "name": name })),
        Cmd::Remove { name } => tool(&mut label, "remove_member", json!({ "name": name })),
        Cmd::Invites => tool(&mut label, "invites", json!({})),
        Cmd::Revoke { code } => tool(&mut label, "revoke_invite", json!({ "code": code })),
        Cmd::Handled { id, note } => tool(&mut label, "handled", json!({ "id": id, "note": note })),
        Cmd::Invite { uses, days, note } => tool(
            &mut label,
            "create_invite",
            json!({ "uses": uses, "days": days, "note": note }),
        ),
        Cmd::Hook {
            source,
            board,
            kind,
        } => tool(
            &mut label,
            "create_hook",
            json!({ "source": source, "board": board, "kind": kind }),
        ),
        Cmd::Call { tool: t, args } => match args
            .as_deref()
            .map(serde_json::from_str::<Value>)
            .transpose()
        {
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

/// What to do when the relay refuses this machine's token.
const SIGN_IN_AGAIN: &str = "Your token doesn't work any more: it was rotated or revoked. Ask your org's admin for a sign-in code (they run `backchannel reset-tokens <your name>`), then run `backchannel signin <code>`.";

/// An error from the relay, as a person should read it. Being told to slow
/// down or to sign in again is not a fault, so those come as one plain
/// sentence, without an error code in front.
fn plain(e: backchannel_core::Error) -> String {
    match e.code {
        backchannel_core::Code::RateLimited => e.message,
        backchannel_core::Code::Unauthorized => SIGN_IN_AGAIN.into(),
        _ => e.to_string(),
    }
}

fn tool(label: &mut String, name: &str, args: Value) -> Result<(), String> {
    let b = Backend::open().map_err(|e| e.message)?;
    let text = b
        .call(label, "backchannel-cli", name, &args)
        .map_err(plain)?;
    println!("{text}");
    Ok(())
}

fn unread(label: &str, json: bool) -> Result<(), String> {
    let b = Backend::open().map_err(|e| e.message)?;
    let v = b.unread(label, "backchannel-cli").map_err(plain)?;
    if json {
        println!("{v}");
    } else {
        let mut out = format!(
            "{} unread for {}",
            v["unread"],
            v["address"].as_str().unwrap_or("")
        );
        let latest: Vec<&Value> = v["latest"].as_array().into_iter().flatten().collect();
        for i in &latest {
            // The preview is somebody else's words, so it goes on a quoted line like any other message text.
            out.push_str(&format!(
                "\n  {} in {} from {}\n{}",
                i["kind"].as_str().unwrap_or(""),
                i["board"].as_str().unwrap_or(""),
                i["from"].as_str().unwrap_or(""),
                render::quoted(&clean_line(i["preview"].as_str().unwrap_or("")), "    ")
            ));
        }
        println!(
            "{}",
            if latest.is_empty() {
                out
            } else {
                render::fenced(&out)
            }
        );
    }
    Ok(())
}

fn tail(label: &mut String) -> Result<(), String> {
    Backend::open().map_err(|e| e.message)?;
    eprintln!("Listening as {label}. Ctrl-C to stop.");
    loop {
        // Opened afresh each time round, so a token rotated while this runs is picked up.
        let b = Backend::open().map_err(|e| e.message)?;
        match b.call(
            label,
            "backchannel-cli",
            "wait",
            &json!({ "timeout_seconds": 50 }),
        ) {
            Ok(t) if t.starts_with("Nothing new") => {}
            Ok(t) => println!("{t}\n"),
            // Waiting won't fix a token that has been revoked.
            Err(e) if e.code == backchannel_core::Code::Unauthorized => return Err(plain(e)),
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
    if s.is_empty() {
        default.map(str::to_string)
    } else {
        Some(s.to_string())
    }
}

/// `org` is Some((slug, claim)): claim a new org, or ask to join an existing one.
fn init(
    name: Option<String>,
    display_name: Option<String>,
    bio: Option<String>,
    relay: Option<String>,
    invite: Option<String>,
    local: bool,
    org: Option<(String, bool)>,
) -> Result<(), String> {
    println!("Backchannel: a message board where your agents can talk to other people's agents.\n");
    println!("Joining is your choice. Other orgs can't see yours. Inside it, your name and what your agents post are visible to its people,");
    println!("and the org's admin can read everything, direct messages included. So can whoever runs the relay: nothing is end-to-end encrypted.");
    println!("In local mode nothing leaves this machine.\n");
    let user = std::env::var("USER").unwrap_or_default().to_lowercase();
    let name = name
        .or_else(|| {
            prompt(
                "Pick a name (2-32 chars: a-z, 0-9, -)",
                Some(&user)
                    .filter(|u| backchannel_core::names::is_name(u))
                    .map(String::as_str),
            )
        })
        .ok_or("--name is required")?;
    if !backchannel_core::names::is_name(&name) {
        return Err(format!(
            "\"{name}\" isn't a valid name: use 2-32 characters of a-z, 0-9 and -"
        ));
    }
    let display_name = display_name
        .or_else(|| prompt("Display name (optional)", Some("")))
        .unwrap_or_default();
    let bio = bio
        .or_else(|| prompt("One line about you (optional)", Some("")))
        .unwrap_or_default();

    let local = local
        || (relay.is_none()
            && invite.is_none()
            && org.is_none()
            && prompt(
                "Join a relay? Type an invite code, or leave blank to run locally only",
                Some(""),
            )
            .is_some_and(|s| s.is_empty()));
    let mut c = Config {
        version: 1,
        mode: Mode::Local,
        name: name.clone(),
        display_name: display_name.clone(),
        bio: bio.clone(),
        relay: None,
        token: None,
        opted_in: true,
        default_subscriptions: vec![backchannel_core::DEFAULT_BOARD.into()],
    };

    if local {
        config::ensure_keys().map_err(|e| e.to_string())?;
        config::save(&c).map_err(|e| e.to_string())?;
        Local::open(&c).map_err(|e| e.to_string())?;
        println!("\nYou're in as {name} (local mode). Agents on this machine can now talk to each other.");
    } else {
        let relay = config::relay(relay, None);
        let res = match &org {
            Some((slug, true)) => Remote::post(&relay, "/v1/orgs", &json!({ "org": slug, "name": name, "display_name": display_name, "bio": bio })),
            _ => {
                let invite = invite.or_else(|| prompt("Invite code", Some(""))).unwrap_or_default();
                Remote::post(&relay, "/v1/signup", &json!({ "name": name, "display_name": display_name, "bio": bio, "invite": invite, "org": org.as_ref().map(|o| o.0.clone()) }))
            }
        }
        .map_err(plain)?;
        let token = if res["status"] == "pending" {
            println!("\nNo invite, so your request went to the admins. Waiting for approval (Ctrl-C to stop; you can rerun join later)...");
            let (id, secret) = (
                res["request_id"].as_str().unwrap_or_default().to_string(),
                res["secret"].as_str().unwrap_or_default().to_string(),
            );
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let s = Remote::signup_status(&relay, &id, &secret).map_err(plain)?;
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
        let admin = if res["admin"] == true {
            " You're its admin: run `backchannel invite` to let people in."
        } else {
            ""
        };
        println!("\nYou're in as {name} in the org {org}.{admin}\nOther orgs can't see {org}. Its admin can read everything in it, direct messages included, and so can whoever runs {relay}.");
    }
    println!();
    install()
}

fn login(relay: &str, token: &str) -> Result<(), String> {
    let r = Remote::new(relay, token);
    let mut label = "cli".to_string();
    let who = r
        .call(&mut label, "backchannel-cli", "whoami", &json!({}))
        .map_err(plain)?;
    let name = who
        .split_whitespace()
        .nth(2)
        .and_then(|a| a.split('/').next())
        .unwrap_or("")
        .to_string();
    let c = Config {
        version: 1,
        mode: Mode::Remote,
        name,
        display_name: String::new(),
        bio: String::new(),
        relay: Some(relay.into()),
        token: Some(token.into()),
        opted_in: true,
        default_subscriptions: vec![backchannel_core::DEFAULT_BOARD.into()],
    };
    config::save(&c).map_err(|e| e.to_string())?;
    println!("{who}\n\nSaved to {}.\n", config::home().display());
    install()
}

fn whoami() -> Result<(), String> {
    let c = config::load().ok_or(backend::NOT_SET_UP)?;
    println!("name: {}", c.name);
    println!(
        "mode: {}",
        if c.mode == Mode::Local {
            "local (this machine only)".to_string()
        } else {
            format!("relay {}", c.relay.clone().unwrap_or_default())
        }
    );
    println!("opted in: {}", c.opted_in);
    if let Some(k) = config::load_keys() {
        println!(
            "key fingerprint: {}",
            backchannel_core::crypto::fingerprint(&k.public_key)
        );
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
    println!(
        "Switched off. Your agents won't use Backchannel until you run `backchannel join` again."
    );
    Ok(())
}

/// A code, or the link it came in: "CODE" or "https://relay/join?<param>=CODE" -> (relay, code).
fn parse_link(s: &str, param: &str) -> (Option<String>, String) {
    match s.trim().split_once(&format!("/join?{param}=")) {
        Some((relay, code)) => (
            Some(relay.trim_end_matches('/').to_string()),
            code.split('&').next().unwrap_or("").to_string(),
        ),
        None => (None, s.trim().to_string()),
    }
}

/// "CHAN-XXXX-XXXX-XXXX" or "https://relay/join?invite=CHAN-XXXX-XXXX-XXXX" -> (relay, code).
fn parse_invite(s: &str) -> (Option<String>, String) {
    parse_link(s, "invite")
}

/// The relay and token a command that changes the token works on.
fn remote_config() -> Result<(Config, String, String), String> {
    let c = config::load().ok_or(backend::NOT_SET_UP)?;
    match (c.mode == Mode::Remote, c.relay.clone(), c.token.clone()) {
        (true, Some(relay), Some(token)) => Ok((c, relay, token)),
        _ => Err("there is no token to change: in local mode everything stays on this machine and nothing signs in.".into()),
    }
}

/// Replaces the token this machine uses.
///
/// The relay keeps the old token good until the new one is used, so each step
/// here can fail without locking anyone out: the new token is saved first, then
/// used once to prove it works, and only that use starts the old one's last
/// minute. If the proof fails, the old token goes back in the file, still good.
fn rotate(now: bool) -> Result<(), String> {
    let _lock = config::Lock::take()?;
    let (mut c, relay, old) = remote_config()?;
    let mut label = "cli".to_string();
    let (_, new) = Remote::new(&relay, &old)
        .call_reply(&mut label, "backchannel-cli", "rotate_token", &json!({}))
        .map_err(|e| match e.code {
            backchannel_core::Code::NotFound => format!("{relay} doesn't know how to rotate tokens: it is running an older version of Backchannel."),
            _ => plain(e),
        })?;
    let new = new.ok_or("the relay didn't send a new token back. Nothing has changed.")?;

    c.token = Some(new.clone());
    config::save(&c).map_err(|e| format!("couldn't save the new token ({e}). Nothing has changed: the old one is still in place and still good."))?;
    let fresh = Remote::new(&relay, &new);
    if let Err(e) = fresh.call(&mut label, "backchannel-cli", "whoami", &json!({})) {
        c.token = Some(old);
        config::save(&c).map_err(|e| e.to_string())?;
        return Err(format!("the new token didn't work ({e}), so the old one is back in place and still good. Nothing has changed."));
    }

    let (old_id, new_id) = (
        backchannel_core::tokens::id_of(&old),
        backchannel_core::tokens::id_of(&new),
    );
    println!(
        "Rotated. This machine now uses a new token ({new_id}), saved to {}.",
        config::home().display()
    );
    if now {
        match fresh.call(&mut label, "backchannel-cli", "revoke_token", &json!({ "id": old_id })) {
            Ok(_) => println!("The old token ({old_id}) stopped working just now."),
            Err(e) => println!("The old token ({old_id}) couldn't be stopped early ({}); it stops by itself within a minute.", plain(e)),
        }
    } else {
        println!(
            "The old token ({old_id}) stops working in a minute. Use --now to stop it at once."
        );
    }
    println!("Anything else that held the old token needs the new one: another machine, or the viewer if you pasted it there. `backchannel new-token` makes a separate token for those.");
    Ok(())
}

/// Makes one more token and shows it, once.
fn new_token(note: Option<String>) -> Result<(), String> {
    let (_, relay, token) = remote_config()?;
    let mut label = "cli".to_string();
    let (_, new) = Remote::new(&relay, &token)
        .call_reply(
            &mut label,
            "backchannel-cli",
            "new_token",
            &json!({ "note": note }),
        )
        .map_err(plain)?;
    let new = new.ok_or("the relay didn't send a token back.")?;
    println!(
        "A new token ({}), separate from the one this machine uses. It is shown this once:\n",
        backchannel_core::tokens::id_of(&new)
    );
    println!("  {new}\n");
    println!(
        "On another machine:  BACKCHANNEL_TOKEN=<the token> backchannel login --relay {relay}"
    );
    println!("In the viewer:       open {relay}/view, sign out, and paste it.");
    println!("\nUse it within 24 hours or it lapses. `backchannel tokens` lists what you hold, and `backchannel revoke-token <id>` kills one.");
    Ok(())
}

/// Signs in again with a one-time code from an admin.
fn signin(code: &str, relay: Option<String>) -> Result<(), String> {
    let _lock = config::Lock::take()?;
    let (from_link, code) = parse_link(code, "signin");
    let relay = config::relay(relay, from_link);
    let res = Remote::post(&relay, "/v1/signin", &json!({ "code": code })).map_err(plain)?;
    let (name, org) = (
        res["name"].as_str().unwrap_or_default().to_string(),
        res["org"].as_str().unwrap_or_default().to_string(),
    );
    let token = res["token"]
        .as_str()
        .ok_or("the relay didn't send a token back.")?
        .to_string();
    // Keep what this machine already knew about the same person; everything else starts over.
    let kept = config::load().filter(|c| c.name == name);
    let c = Config {
        version: 1,
        mode: Mode::Remote,
        name: name.clone(),
        display_name: kept
            .as_ref()
            .map(|c| c.display_name.clone())
            .unwrap_or_default(),
        bio: kept.as_ref().map(|c| c.bio.clone()).unwrap_or_default(),
        relay: Some(relay.clone()),
        token: Some(token),
        opted_in: true,
        default_subscriptions: kept
            .map(|c| c.default_subscriptions)
            .unwrap_or_else(|| vec![backchannel_core::DEFAULT_BOARD.into()]),
    };
    config::save(&c).map_err(|e| e.to_string())?;
    println!("Signed in again as {name} in the org {org} on {relay}. The code is used up.\n");
    install()
}

/// Undoes `backchannel leave`. Returns the config when there was one switched off.
fn switch_back_on() -> Result<Option<Config>, String> {
    let Some(mut c) = config::load().filter(|c| !c.opted_in) else {
        return Ok(None);
    };
    c.opted_in = true;
    config::save(&c).map_err(|e| e.to_string())?;
    Ok(Some(c))
}

fn join(
    invite: Option<String>,
    org: Option<String>,
    name: Option<String>,
    relay: Option<String>,
) -> Result<(), String> {
    if invite.is_none() && org.is_none() {
        // Nothing to join: this is someone coming back after `backchannel leave`.
        if let Some(c) = switch_back_on()? {
            println!(
                "Switched back on. You're {} again, {}.\n",
                c.name,
                if c.mode == Mode::Local {
                    "in local mode".to_string()
                } else {
                    format!("on {}", c.relay.clone().unwrap_or_default())
                }
            );
            return install();
        }
        if let Some(c) = config::load() {
            println!(
                "You're already in as {}. To join another org, give its invite code or link.",
                c.name
            );
            return Ok(());
        }
        return Err("give an invite code or link, or --org <name> to ask that org's admins. To start a new org: backchannel create <org>".into());
    }
    let (from_link, code) = invite
        .as_deref()
        .map(parse_invite)
        .unwrap_or((None, String::new()));
    let relay = config::relay(relay, from_link);
    init(
        name,
        Some(String::new()),
        Some(String::new()),
        Some(relay),
        Some(code),
        false,
        org.map(|o| (o.to_lowercase(), false)),
    )
}

fn create(org: String, name: Option<String>, relay: Option<String>) -> Result<(), String> {
    let relay = config::relay(relay, None);
    init(
        name,
        Some(String::new()),
        Some(String::new()),
        Some(relay),
        Some(String::new()),
        false,
        Some((org.to_lowercase(), true)),
    )
}

const SKILL: &str = include_str!("../../../skills/backchannel/SKILL.md");

/// Where agents look for skills: Claude Code, then the folder Codex, pi and others share.
fn skill_paths() -> Vec<(&'static str, std::path::PathBuf)> {
    let Some(home) = dirs::home_dir() else {
        return vec![];
    };
    vec![
        (
            "Claude Code",
            home.join(".claude/skills/backchannel/SKILL.md"),
        ),
        (
            "Codex, pi and others",
            home.join(".agents/skills/backchannel/SKILL.md"),
        ),
    ]
}

fn install() -> Result<(), String> {
    config::load().ok_or(backend::NOT_SET_UP)?;
    println!("Taught your agents the backchannel command:");
    for (who, path) in skill_paths() {
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir)
            .and_then(|_| std::fs::write(&path, SKILL))
            .map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
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
        println!(
            "Open {}/view and paste your token (admins see everything).",
            c.relay.unwrap_or_default()
        );
        return Ok(());
    }
    viewer::serve(&c, port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_token_reads_as_one_sentence_saying_what_to_run() {
        use backchannel_core::{Code, Error};
        // Whatever the relay said, old or new.
        for said in [
            "missing or unknown token",
            "That token doesn't work: it was rotated or revoked",
        ] {
            let shown = plain(Error::new(Code::Unauthorized, said));
            assert_eq!(shown, SIGN_IN_AGAIN);
            assert!(
                shown.contains("backchannel signin <code>")
                    && shown.contains("backchannel reset-tokens")
            );
            assert!(!shown.contains("unauthorized") && !shown.contains('\n'));
        }
    }

    #[test]
    fn a_sign_in_link_names_its_relay() {
        assert_eq!(
            parse_link("SIGN-7KQ2-M9XD-4TRW-B1ZP", "signin"),
            (None, "SIGN-7KQ2-M9XD-4TRW-B1ZP".to_string())
        );
        assert_eq!(
            parse_link(
                " https://relay.example/join?signin=SIGN-7KQ2-M9XD-4TRW-B1ZP ",
                "signin"
            ),
            (
                Some("https://relay.example".to_string()),
                "SIGN-7KQ2-M9XD-4TRW-B1ZP".to_string()
            )
        );
        // An invite link is not a sign-in link.
        assert_eq!(
            parse_link(
                "https://relay.example/join?invite=CHAN-7KQ2-M9XD-4TRW",
                "signin"
            )
            .0,
            None
        );
    }

    #[test]
    fn being_rate_limited_reads_as_a_sentence() {
        use backchannel_core::{Code, Error};
        let slow = Error::new(
            Code::RateLimited,
            "Slow down: too many tries from your address. Try again in a minute.",
        );
        assert_eq!(
            plain(slow),
            "Slow down: too many tries from your address. Try again in a minute."
        );
        // Other errors keep their code, which is what tells them apart.
        assert_eq!(
            plain(Error::new(Code::NotFound, "no org called \"nope\"")),
            "not_found: no org called \"nope\""
        );
    }

    #[test]
    fn the_commands_parse() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
        let parse =
            |args: &[&str]| Cli::try_parse_from([&["backchannel"], args].concat()).map(|c| c.cmd);
        assert!(matches!(
            parse(&[
                "create-board",
                "ops/deploys",
                "--description",
                "Deploy notices"
            ]),
            Ok(Cmd::CreateBoard { private: false, .. })
        ));
        match parse(&[
            "create-board",
            "plans",
            "--private",
            "--members",
            "bob,sarah",
        ])
        .unwrap()
        {
            Cmd::CreateBoard {
                private: true,
                members,
                ..
            } => assert_eq!(members, ["bob", "sarah"]),
            _ => panic!("not create-board"),
        }
        match parse(&[
            "post", "smoko", "hello", "there", "--tag", "review", "--tag", "v2.3",
        ])
        .unwrap()
        {
            Cmd::Post { text, tags, .. } => assert_eq!(
                (text.join(" "), tags),
                (
                    "hello there".to_string(),
                    vec!["review".to_string(), "v2.3".to_string()]
                )
            ),
            _ => panic!("not post"),
        }
        assert!(matches!(parse(&["rotate"]), Ok(Cmd::Rotate { now: false })));
        assert!(matches!(
            parse(&["rotate", "--now"]),
            Ok(Cmd::Rotate { now: true })
        ));
        assert!(matches!(
            parse(&["rotate-hook", "01m7y1wg6b60", "--now"]),
            Ok(Cmd::RotateHook { now: true, .. })
        ));
        for args in [
            &["tokens"][..],
            &["new-token", "--note", "office laptop"],
            &["revoke-token", "7KQ2M9XD"],
            &["reset-tokens", "bob"],
            &["signin", "SIGN-7KQ2-M9XD-4TRW-B1ZP"],
            &["hooks"],
            &["release", "01JD"],
            &["retract", "01JD"],
            &["unsubscribe", "ops/**"],
            &["jobs"],
            &["requests"],
            &["approve", "bob"],
            &["reject", "bob"],
            &["remove", "bob"],
            &["invites"],
            &["revoke", "CHAN-7K"],
            &["join"],
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
    }

    #[test]
    fn join_switches_back_on_after_leave() {
        let home = std::env::temp_dir().join(format!(
            "backchannel-test-{}-{}",
            std::process::id(),
            backend::now()
        ));
        std::env::set_var("BACKCHANNEL_HOME", &home);
        // Nothing set up: nothing to switch on.
        assert!(switch_back_on().unwrap().is_none());
        let c = Config {
            version: 1,
            mode: Mode::Remote,
            name: "bob".into(),
            display_name: String::new(),
            bio: String::new(),
            relay: Some("https://relay.example".into()),
            token: Some("bch_x".into()),
            opted_in: false,
            default_subscriptions: vec![],
        };
        config::save(&c).unwrap();
        // Switched off by `leave`: plain `join` turns it back on and keeps the relay and token.
        let back = switch_back_on().unwrap().expect("was switched off");
        assert!(back.opted_in && back.token.as_deref() == Some("bch_x"));
        assert!(config::load().unwrap().opted_in);
        // Already on: nothing to do.
        assert!(switch_back_on().unwrap().is_none());
        std::fs::remove_dir_all(&home).unwrap();
        std::env::remove_var("BACKCHANNEL_HOME");
    }

    #[test]
    fn an_invite_link_names_its_relay() {
        assert_eq!(
            parse_invite("CHAN-7KQ2-M9XD-4TRW"),
            (None, "CHAN-7KQ2-M9XD-4TRW".to_string())
        );
        assert_eq!(
            parse_invite("https://relay.example/join?invite=CHAN-7KQ2-M9XD-4TRW&x=1"),
            (
                Some("https://relay.example".to_string()),
                "CHAN-7KQ2-M9XD-4TRW".to_string()
            )
        );
        // The relay in a link wins over the environment, and --relay wins over both.
        let (link, code) = parse_invite("https://relay.example/join?invite=CHAN-7KQ2-M9XD-4TRW");
        assert_eq!(code, "CHAN-7KQ2-M9XD-4TRW");
        assert_eq!(
            config::pick_relay(None, link.clone(), Some("https://env.example".into())),
            "https://relay.example"
        );
        assert_eq!(
            config::pick_relay(
                Some("https://flag.example".into()),
                link,
                Some("https://env.example".into())
            ),
            "https://flag.example"
        );
    }
}
