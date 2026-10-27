//! Login, with the session persisted inside the SDK's SQLite store.
//!
//! The session (access token, device ID) lives next to the crypto state, so the
//! two can't drift apart: a fresh login means a new device, which can't decrypt
//! anything the old one received. The store hashes custom-value keys but keeps
//! the values in clear, so the session is sealed with a `StoreCipher` of our own,
//! itself exported under `STORE_PASSPHRASE`.
//!
//! A restored device can lose its keys on the server (deleted from another
//! client, server restored from a backup). The crypto store still believes they
//! are uploaded and never re-uploads them, so nobody can encrypt to us anymore:
//! we log that device out, wipe the store and start over with a new one.

use anyhow::Context;
use matrix_sdk::{
    Client,
    authentication::matrix::MatrixSession,
    config::RequestConfig,
    ruma::{api::client::keys::get_keys, assign},
};
use matrix_sdk_store_encryption::StoreCipher;
use tracing::{info, warn};

use crate::config::Config;

const CIPHER_KEY: &[u8; 13] = b"tekton.cipher";
const SESSION_KEY: &[u8; 14] = b"tekton.session";

/// Restores the saved session, or logs in with the password on first start or
/// when the restored device lost its keys on the server.
pub async fn login(config: &Config) -> anyhow::Result<Client> {
    let (client, cipher) = open(config).await?;
    let Some(sealed) = client.state_store().get_custom_value(SESSION_KEY).await? else {
        password_login(config, &client, &cipher).await?;
        return Ok(client);
    };
    let session: MatrixSession = cipher.decrypt_value(&sealed).context("unsealing session")?;
    client.restore_session(session).await?;
    if has_device_keys(&client).await? {
        info!("restored session");
        return Ok(client);
    }

    // Checked before wiping, so a missing password doesn't cost us the old session.
    credentials(config)?;
    warn!("our device keys are gone from the server, replacing the device");
    // Deletes the old device server-side; the wipe below happens regardless.
    if let Err(err) = client.logout().await {
        warn!(%err, "logging out the old device");
    }
    drop(client);
    let store = config.data_dir.join("store");
    std::fs::remove_dir_all(&store).with_context(|| format!("wiping {}", store.display()))?;
    let (client, cipher) = open(config).await?;
    password_login(config, &client, &cipher).await?;
    Ok(client)
}

/// Builds the client on the SQLite store, and loads our cipher from it.
///
/// Without a retry limit the SDK never retries network failures (timeouts,
/// refused connections), so one blip fails the request for good, and with it
/// the one-shot E2EE setup (cross-signing, backups, recovery) after login.
async fn open(config: &Config) -> anyhow::Result<(Client, StoreCipher)> {
    let client = Client::builder()
        .homeserver_url(&config.homeserver)
        .request_config(RequestConfig::new().retry_limit(5))
        .sqlite_store(
            config.data_dir.join("store"),
            Some(&config.store_passphrase),
        )
        .build()
        .await?;
    let cipher = cipher(&client, &config.store_passphrase).await?;
    Ok((client, cipher))
}

/// Logs in as a new device and saves the sealed session.
async fn password_login(
    config: &Config,
    client: &Client,
    cipher: &StoreCipher,
) -> anyhow::Result<()> {
    let (user, password) = credentials(config)?;
    client
        .matrix_auth()
        .login_username(user, password)
        .initial_device_display_name("tekton-matrix-bot")
        .await?;
    let session = client
        .matrix_auth()
        .session()
        .context("no session after login")?;
    let sealed = cipher.encrypt_value(&session)?;
    client
        .state_store()
        .set_custom_value(SESSION_KEY, sealed)
        .await?;
    info!("logged in, saved session");
    Ok(())
}

fn credentials(config: &Config) -> anyhow::Result<(&str, &str)> {
    let user = config
        .user
        .as_deref()
        .context("MATRIX_USER is required to log in")?;
    let password = config
        .password
        .as_deref()
        .context("MATRIX_PASSWORD is required to log in")?;
    Ok((user, password))
}

/// Whether the server still publishes keys for our device. Errors (network,
/// server) propagate rather than count as missing, so they never wipe the store.
async fn has_device_keys(client: &Client) -> anyhow::Result<bool> {
    let user = client.user_id().context("no user ID after restore")?;
    let device = client.device_id().context("no device ID after restore")?;
    let request = assign!(get_keys::v3::Request::new(), {
        device_keys: [(user.to_owned(), vec![device.to_owned()])].into(),
    });
    let response = client
        .send(request)
        .await
        .context("querying our device keys")?;
    Ok(response
        .device_keys
        .get(user)
        .is_some_and(|devices| devices.contains_key(device)))
}

/// Loads our cipher from the store, creating it on first start.
async fn cipher(client: &Client, passphrase: &str) -> anyhow::Result<StoreCipher> {
    let store = client.state_store();
    if let Some(exported) = store.get_custom_value(CIPHER_KEY).await? {
        return Ok(StoreCipher::import(passphrase, &exported)?);
    }
    let cipher = StoreCipher::new()?;
    store
        .set_custom_value(CIPHER_KEY, cipher.export(passphrase)?)
        .await?;
    Ok(cipher)
}
