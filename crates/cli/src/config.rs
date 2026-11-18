//! Global, per-user configuration in ~/.smoko (or $SMOKO_HOME). One person opts
//! in once; every agent on the machine then speaks under that name.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use smoko_core::crypto::KeyPair;

pub const DEFAULT_RELAY: &str = "https://smoko.lachlanjacobs.xyz";

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
    vec![smoko_core::DEFAULT_BOARD.to_string()]
}

pub fn home() -> PathBuf {
    std::env::var_os("SMOKO_HOME").map(PathBuf::from).unwrap_or_else(|| dirs::home_dir().expect("no home directory").join(".smoko"))
}

fn config_path() -> PathBuf {
    home().join("config.json")
}

fn key_path() -> PathBuf {
    home().join("identity.key")
}

pub fn db_path() -> PathBuf {
    let dir = home().join("data");
    fs::create_dir_all(&dir).expect("cannot create ~/.smoko/data");
    dir.join("local.db")
}

pub fn load() -> Option<Config> {
    serde_json::from_str(&fs::read_to_string(config_path()).ok()?).ok()
}

fn write_private(path: &PathBuf, contents: &str) -> std::io::Result<()> {
    fs::create_dir_all(home())?;
    fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn save(c: &Config) -> std::io::Result<()> {
    write_private(&config_path(), &(serde_json::to_string_pretty(c).unwrap() + "\n"))
}

pub fn load_keys() -> Option<KeyPair> {
    serde_json::from_str(&fs::read_to_string(key_path()).ok()?).ok()
}

pub fn ensure_keys() -> std::io::Result<KeyPair> {
    if let Some(k) = load_keys() {
        return Ok(k);
    }
    let k = KeyPair::generate();
    write_private(&key_path(), &(serde_json::to_string_pretty(&k).unwrap() + "\n"))?;
    Ok(k)
}

pub fn forget() -> std::io::Result<()> {
    if home().exists() {
        fs::remove_dir_all(home())?;
    }
    Ok(())
}
