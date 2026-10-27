//! Kubernetes probes, served over plain HTTP on `HEALTH_ADDR`.
//!
//! - `/health/live`: fails once syncing has been failing for `LIVE_STALE`, so a
//!   bot stuck on an error it can't recover from (revoked token, …) gets restarted.
//!   Before the first sync it always passes: login has its own timeouts.
//! - `/health/ready`: passes once the first sync is done and the last one is
//!   recent. A long-poll returns at least every 30s, even with nothing new.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Context;
use axum::{Router, extract::State, http::StatusCode, routing::get};
use tokio::net::TcpListener;
use tracing::{error, info};

const READY_STALE: Duration = Duration::from_secs(2 * 60);
const LIVE_STALE: Duration = Duration::from_secs(5 * 60);

/// When the last sync succeeded; `None` until the first one.
#[derive(Clone, Default)]
pub struct Health(Arc<Mutex<Option<Instant>>>);

impl Health {
    pub fn synced(&self) {
        *self.0.lock().unwrap() = Some(Instant::now());
    }

    fn since_sync(&self) -> Option<Duration> {
        self.0.lock().unwrap().map(|at| at.elapsed())
    }
}

/// Binds now, so a taken port fails startup, and serves in the background.
pub async fn serve(addr: SocketAddr, health: Health) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding health server to {addr}"))?;
    let app = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .with_state(health);
    info!(%addr, "health server listening");
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            error!(%err, "health server stopped");
        }
    });
    Ok(())
}

async fn live(State(health): State<Health>) -> StatusCode {
    status(health.since_sync().is_none_or(|age| age < LIVE_STALE))
}

async fn ready(State(health): State<Health>) -> StatusCode {
    status(health.since_sync().is_some_and(|age| age < READY_STALE))
}

fn status(ok: bool) -> StatusCode {
    if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
