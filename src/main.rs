mod commands;
mod config;
mod handlers;
mod session;

use anyhow::Context;
use matrix_sdk::config::SyncSettings;
use tokio::signal::unix::{SignalKind, signal};
use tracing::info;

use crate::config::Config;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,matrix_sdk=warn".into()),
        )
        .init();

    let config = Config::from_env()?;
    let client = session::login(&config).await?;

    // Handlers are registered after the first sync so the bot doesn't answer the backlog.
    let response = client.sync_once(SyncSettings::default()).await?;
    client.add_event_handler(handlers::on_invite);
    client.add_event_handler(handlers::on_message);
    let user = client.user_id().context("no user ID after login")?;
    info!(%user, "ready");

    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        res = client.sync(SyncSettings::default().token(response.next_batch)) => res?,
        _ = sigterm.recv() => info!("SIGTERM, shutting down"),
        _ = tokio::signal::ctrl_c() => info!("SIGINT, shutting down"),
    }
    Ok(())
}
