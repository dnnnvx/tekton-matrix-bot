//! Environment configuration, read and validated once at startup.
//!
//! Every variable can also be given as `<NAME>_FILE` pointing at a file holding
//! the value (Docker/Kubernetes secrets); the file wins over the plain variable.

use std::{env, fs, net::SocketAddr, path::PathBuf};

use anyhow::Context;

pub struct Config {
    pub homeserver: String,
    pub data_dir: PathBuf,
    pub store_passphrase: String,
    /// Only needed on first start, when there's no saved session yet.
    pub user: Option<String>,
    pub password: Option<String>,
    pub health_addr: SocketAddr,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            homeserver: var("MATRIX_HOMESERVER")?.context("MATRIX_HOMESERVER is required")?,
            data_dir: var("DATA_DIR")?.unwrap_or_else(|| "/data".into()).into(),
            store_passphrase: var("STORE_PASSPHRASE")?.context("STORE_PASSPHRASE is required")?,
            user: var("MATRIX_USER")?,
            password: var("MATRIX_PASSWORD")?,
            health_addr: var("HEALTH_ADDR")?
                .as_deref()
                .unwrap_or("0.0.0.0:8080")
                .parse()
                .context("HEALTH_ADDR must be <ip>:<port>")?,
        })
    }
}

fn var(name: &str) -> anyhow::Result<Option<String>> {
    let file_var = format!("{name}_FILE");
    if let Ok(path) = env::var(&file_var) {
        let value = fs::read_to_string(&path).with_context(|| format!("{file_var}={path}"))?;
        return Ok(Some(value.trim_end_matches(['\r', '\n']).to_owned()));
    }
    Ok(env::var(name).ok().filter(|v| !v.is_empty()))
}
