//! Environment configuration, read and validated once at startup.
//!
//! Every variable can also be given as `<NAME>_FILE` pointing at a file holding
//! the value (Docker/Kubernetes secrets); the file wins over the plain variable.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use std::{env, fs};

use anyhow::Context;
use matrix_sdk::ruma::OwnedRoomId;

pub struct Config {
    pub homeserver: String,
    pub data_dir: PathBuf,
    pub store_passphrase: String,
    /// Only needed on first start, when there's no saved session yet.
    pub user: Option<String>,
    pub password: Option<String>,
    /// Keeps a copy of the cross-signing keys on the server when set.
    pub recovery_passphrase: Option<String>,
    pub health_addr: SocketAddr,
    pub tekton_dashboard_url: String,
    /// Where cluster commands are allowed and CI alerts go.
    pub ops_room: OwnedRoomId,
    /// How often the pending tofu plan is checked.
    pub tofu_poll: Duration,
    /// A CI run tracked this long is forgotten, in case its end was never seen.
    pub tekton_gc: Duration,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            homeserver: var("MATRIX_HOMESERVER")?.context("MATRIX_HOMESERVER is required")?,
            data_dir: var("DATA_DIR")?.unwrap_or_else(|| "/data".into()).into(),
            store_passphrase: var("STORE_PASSPHRASE")?.context("STORE_PASSPHRASE is required")?,
            user: var("MATRIX_USER")?,
            password: var("MATRIX_PASSWORD")?,
            recovery_passphrase: var("RECOVERY_PASSPHRASE")?,
            health_addr: var("HEALTH_ADDR")?
                .as_deref()
                .unwrap_or("0.0.0.0:8080")
                .parse()
                .context("HEALTH_ADDR must be <ip>:<port>")?,
            tekton_dashboard_url: var("TEKTON_DASHBOARD_URL")?
                .context("TEKTON_DASHBOARD_URL is required")?,
            ops_room: var("OPS_ROOM")?
                .context("OPS_ROOM is required")?
                .parse()
                .context("OPS_ROOM must be a room ID")?,
            tofu_poll: Duration::from_secs(60 * positive("TOFU_POLL_MINUTES", 30)?),
            tekton_gc: Duration::from_secs(3600 * positive("TEKTON_GC_HOURS", 24)?),
        })
    }
}

fn positive(name: &str, default: u64) -> anyhow::Result<u64> {
    let Some(value) = var(name)? else {
        return Ok(default);
    };
    value
        .parse()
        .ok()
        .filter(|n| *n > 0)
        .with_context(|| format!("{name} must be a whole number above 0"))
}

fn var(name: &str) -> anyhow::Result<Option<String>> {
    let file_var = format!("{name}_FILE");
    if let Ok(path) = env::var(&file_var) {
        let value = fs::read_to_string(&path).with_context(|| format!("{file_var}={path}"))?;
        return Ok(Some(value.trim_end_matches(['\r', '\n']).to_owned()));
    }
    Ok(env::var(name).ok().filter(|v| !v.is_empty()))
}
