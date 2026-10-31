mod alerts;
mod cluster;
mod commands;
mod config;
mod handlers;
mod health;
mod session;

use anyhow::Context;
use matrix_sdk::{Client, LoopCtrl, config::SyncSettings};
use tokio::signal::unix::{SignalKind, signal};
use tracing::{info, warn};

use crate::{cluster::Cluster, config::Config, health::Health};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,matrix_sdk=warn".into()),
        )
        .init();

    let config = Config::from_env()?;
    let cluster = Cluster::connect(&config).await?;
    let health = Health::default();
    health::serve(config.health_addr, health.clone()).await?;
    let client = session::login(&config).await?;

    // Handlers are registered after the first sync so the bot doesn't answer the backlog.
    let response = client.sync_once(SyncSettings::default()).await?;
    health.synced();
    // Optional, and the bot works without it: a failure doesn't stop it from starting.
    if let Some(passphrase) = &config.recovery_passphrase
        && let Err(err) = session::recover(&client, passphrase).await
    {
        warn!("recovery failed: {err:#}");
    }
    client.add_event_handler_context(cluster.clone());
    client.add_event_handler(handlers::on_invite);
    client.add_event_handler(handlers::on_message);
    tokio::spawn(alerts::watch_ci(client.clone(), cluster));
    let user = client.user_id().context("no user ID after login")?;
    info!(%user, "ready");

    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        res = sync(&client, &health, response.next_batch) => res?,
        _ = sigterm.recv() => info!("SIGTERM, shutting down"),
        _ = tokio::signal::ctrl_c() => info!("SIGINT, shutting down"),
    }
    Ok(())
}

/// Syncs forever. A failed sync is logged and retried instead of ending the
/// loop; if failures last, the liveness probe gets the pod restarted.
async fn sync(client: &Client, health: &Health, token: String) -> matrix_sdk::Result<()> {
    let settings = SyncSettings::default().token(token);
    client
        .sync_with_result_callback(settings, |result| async move {
            match result {
                Ok(_) => health.synced(),
                Err(err) => warn!(%err, "sync failed"),
            }
            Ok(LoopCtrl::Continue)
        })
        .await
}
