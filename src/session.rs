//! Login, with the session persisted inside the SDK's SQLite store.
//!
//! The session (access token, device ID) lives next to the crypto state, so the
//! two can't drift apart: a fresh login means a new device, which can't decrypt
//! anything the old one received. The store hashes custom-value keys but keeps
//! the values in clear, so the session is sealed with a `StoreCipher` of our own,
//! itself exported under `STORE_PASSPHRASE`.

use anyhow::Context;
use matrix_sdk::{Client, authentication::matrix::MatrixSession};
use matrix_sdk_store_encryption::StoreCipher;
use tracing::info;

use crate::config::Config;

const CIPHER_KEY: &[u8; 13] = b"tekton.cipher";
const SESSION_KEY: &[u8; 14] = b"tekton.session";

/// Restores the saved session, or logs in with the password on first start.
pub async fn login(config: &Config) -> anyhow::Result<Client> {
    let client = Client::builder()
        .homeserver_url(&config.homeserver)
        .sqlite_store(
            config.data_dir.join("store"),
            Some(&config.store_passphrase),
        )
        .build()
        .await?;
    let cipher = cipher(&client, &config.store_passphrase).await?;

    if let Some(sealed) = client.state_store().get_custom_value(SESSION_KEY).await? {
        let session: MatrixSession = cipher.decrypt_value(&sealed).context("unsealing session")?;
        client.restore_session(session).await?;
        info!("restored session");
    } else {
        let user = config
            .user
            .as_deref()
            .context("MATRIX_USER is required for the first login")?;
        let password = config
            .password
            .as_deref()
            .context("MATRIX_PASSWORD is required for the first login")?;
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
    }
    Ok(client)
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
