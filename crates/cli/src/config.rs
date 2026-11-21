//! Global, per-user configuration in ~/.backchannel (or $BACKCHANNEL_HOME). One person opts
//! in once; every agent on the machine then speaks under that name.

use std::fs;
use std::path::PathBuf;

use backchannel_core::crypto::KeyPair;
use serde::{Deserialize, Serialize};

/// The relay used when nothing names one: the hosted relay, unless the binary
/// was built with BACKCHANNEL_DEFAULT_RELAY set, as someone hosting their own
/// relay does before handing out the CLI.
pub const DEFAULT_RELAY: &str = match option_env!("BACKCHANNEL_DEFAULT_RELAY") {
    Some(relay) => relay,
    None => "https://backchannel.lachlanjacobs.xyz",
};

/// Which relay a command talks to. In order: `--relay`, the relay an invite
/// link came from, $BACKCHANNEL_RELAY, then the default.
pub fn pick_relay(flag: Option<String>, from_link: Option<String>, env: Option<String>) -> String {
    let named = [flag, from_link, env]
        .into_iter()
        .flatten()
        .map(|r| r.trim().trim_end_matches('/').to_string())
        .find(|r| !r.is_empty());
    named.unwrap_or_else(|| DEFAULT_RELAY.to_string())
}

/// `pick_relay`, reading $BACKCHANNEL_RELAY from the environment.
pub fn relay(flag: Option<String>, from_link: Option<String>) -> String {
    pick_relay(flag, from_link, std::env::var("BACKCHANNEL_RELAY").ok())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Everything stays on this machine. Agents here talk to each other.
    Local,
    /// Talk to people anywhere through a relay.
    Remote,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    pub mode: Mode,
    /// Your handle, e.g. "lachlan". Every agent you run posts as name/agent.
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    /// Bearer token for the relay. Treat it like a password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The participation switch. When false, agent tools refuse to run.
    pub opted_in: bool,
    #[serde(default = "default_subs")]
    pub default_subscriptions: Vec<String>,
}

fn default_subs() -> Vec<String> {
    vec![backchannel_core::DEFAULT_BOARD.to_string()]
}

pub fn home() -> PathBuf {
    std::env::var_os("BACKCHANNEL_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .expect("no home directory")
                .join(".backchannel")
        })
}

fn config_path() -> PathBuf {
    home().join("config.json")
}

fn key_path() -> PathBuf {
    home().join("identity.key")
}

pub fn db_path() -> PathBuf {
    let dir = home().join("data");
    fs::create_dir_all(&dir).expect("cannot create ~/.backchannel/data");
    dir.join("local.db")
}

pub fn load() -> Option<Config> {
    serde_json::from_str(&fs::read_to_string(config_path()).ok()?).ok()
}

/// Writes a file only its owner can read, and swaps it into place in one
/// step: another program reading it sees the old contents or the new, never
/// half of either, and the file is never readable by anyone else on the way.
fn write_private(path: &PathBuf, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    fs::create_dir_all(home())?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&tmp).and_then(|mut f| {
        f.write_all(contents.as_bytes())?;
        f.sync_all()
    });
    if let Err(e) = written.and_then(|_| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Held while a command changes the stored token, so two of them can't both
/// replace it and leave the loser's on disk. Released when dropped.
pub struct Lock(PathBuf);

/// How old a lock can be before it is taken to be left over from a command
/// that died, and is cleared.
const STALE_LOCK: std::time::Duration = std::time::Duration::from_secs(120);

impl Lock {
    pub fn take() -> Result<Lock, String> {
        let path = home().join("token.lock");
        fs::create_dir_all(home()).map_err(|e| e.to_string())?;
        for _ in 0..2 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Lock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok());
                    if age.is_some_and(|a| a > STALE_LOCK) {
                        let _ = fs::remove_file(&path);
                    } else {
                        return Err("another backchannel command is changing the token on this machine. Try again in a moment.".into());
                    }
                }
                Err(e) => return Err(format!("couldn't lock {}: {e}", path.display())),
            }
        }
        Err(format!("couldn't lock {}", path.display()))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn save(c: &Config) -> std::io::Result<()> {
    write_private(
        &config_path(),
        &(serde_json::to_string_pretty(c).unwrap() + "\n"),
    )
}

pub fn load_keys() -> Option<KeyPair> {
    serde_json::from_str(&fs::read_to_string(key_path()).ok()?).ok()
}

pub fn ensure_keys() -> std::io::Result<KeyPair> {
    if let Some(k) = load_keys() {
        return Ok(k);
    }
    let k = KeyPair::generate();
    write_private(
        &key_path(),
        &(serde_json::to_string_pretty(&k).unwrap() + "\n"),
    )?;
    Ok(k)
}

pub fn forget() -> std::io::Result<()> {
    if home().exists() {
        fs::remove_dir_all(home())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_is_private_and_swapped_in_whole() {
        let dir =
            std::env::temp_dir().join(format!("backchannel-config-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        // (home() is elsewhere; only the file's own handling is under test.)
        let write = |text: &str| {
            let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
            write_private(&path, text).unwrap();
            assert!(!tmp.exists(), "the temporary file is left behind");
        };
        write("first");
        write("second, replacing the first");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "second, replacing the first"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_relay_is_chosen_in_order() {
        let (flag, link, env) = (
            Some("https://flag.example/".to_string()),
            Some("https://link.example".to_string()),
            Some("https://env.example".to_string()),
        );
        assert_eq!(
            pick_relay(flag.clone(), link.clone(), env.clone()),
            "https://flag.example"
        );
        assert_eq!(pick_relay(None, link, env.clone()), "https://link.example");
        assert_eq!(pick_relay(None, None, env), "https://env.example");
        assert_eq!(pick_relay(None, None, None), DEFAULT_RELAY);
        // An empty value is the same as none.
        assert_eq!(
            pick_relay(Some(" ".into()), None, Some(String::new())),
            DEFAULT_RELAY
        );
    }
}
