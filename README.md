# tekton-matrix-bot

Matrix bot for the homelab, built on [matrix-rust-sdk](https://github.com/matrix-org/matrix-rust-sdk) (E2EE capable).
It joins rooms it's invited to and answers `!commands` (`!help` lists them) as replies.

## Configuration

| Env | Required | Meaning |
|-----|----------|---------|
| `MATRIX_HOMESERVER` | yes | e.g. `http://tuwunel.matrix.svc.cluster.local:8008` |
| `STORE_PASSPHRASE` | yes | encrypts the SQLite store, including the saved session |
| `MATRIX_USER`, `MATRIX_PASSWORD` | first start only | used to log in when there's no saved session |
| `RECOVERY_PASSPHRASE` | no | keeps the cross-signing keys on the server, encrypted under it, so a new device can sign itself |
| `DATA_DIR` | no (`/data`) | holds `store/` |
| `HEALTH_ADDR` | no (`0.0.0.0:8080`) | where the health probes are served |
| `TEKTON_DASHBOARD_URL` | yes | e.g. `https://tekton.local.casasulgorgo.net`, for links to CI runs |
| `OPS_ROOM` | yes | room ID, the only room where `!ci` and `!tofu` answer and where failed CI runs are posted; use an encrypted, invite-only room |
| `RUST_LOG` | no | tracing filter, default `info,matrix_sdk=warn` |

Any variable can instead be given as `<NAME>_FILE`, pointing at a file holding the value (for mounted secrets).

Everything lives in `store/`: the crypto state and the session (access token, device ID), sealed under `STORE_PASSPHRASE`.
Keep it on a persistent volume. Wiping it means a new login, i.e. a new device, which can't decrypt messages the old one received.

## Device verification

The bot sets up cross-signing by itself and signs its device, so clients don't warn about its messages. The account must not already have cross-signing keys, for example from logging in to it with another client: the bot can't sign itself with keys it doesn't hold, and clients keep warning.

The private keys live in `store/`, so a new device can't sign itself and the warning returns. To avoid that, set `RECOVERY_PASSPHRASE`. On start, the bot stores the keys on the server encrypted under it, along with a server-side backup of room keys, and a later new device fetches them back. Keep it stable and treat it like a password: whoever has it and access to the account can sign devices as the bot. A recovery key or passphrase set up from another client works too, as long as secret storage holds the keys.

## Cluster access

The bot talks to Kubernetes with the in-cluster service account, or the local kubeconfig when run outside the cluster, and only reads:

- `!ci` lists PipelineRuns in `tekton-ci`, linking to the Tekton dashboard. Failed runs are also posted to `OPS_ROOM`, except those that failed while the bot was down.
- `!tofu` shows the pending plan of the `production` Terraform in `flux-system`, and the `kubectl` command to approve it. Applying is left to whoever runs that command.

## Health probes

- `GET /health/live`: 503 once syncing has been failing for 5 minutes, so Kubernetes restarts a bot stuck on an unrecoverable error. Always 200 before the first sync.
- `GET /health/ready`: 200 once the first sync is done and the last one succeeded within 2 minutes.

A failed sync is logged and retried; HTTP requests are retried up to 5 times on network errors.

```yaml
livenessProbe:
  httpGet: { path: /health/live, port: 8080 }
  periodSeconds: 30
readinessProbe:
  httpGet: { path: /health/ready, port: 8080 }
  periodSeconds: 10
```

## Run locally

```bash
mkdir -p data
DATA_DIR=./data MATRIX_HOMESERVER=https://matrix.casasulgorgo.net \
  STORE_PASSPHRASE=… MATRIX_USER=… MATRIX_PASSWORD=… cargo run
```

`cargo fmt`, `clippy` and the tests run in the pre-commit hook (`.githooks/pre-commit`), not in the image build.
The nix dev shell enables it; otherwise run `git config core.hooksPath .githooks`.

## Layout

| File | Role |
|------|------|
| `src/main.rs` | startup and sync loop |
| `src/config.rs` | environment configuration |
| `src/session.rs` | login and session persistence |
| `src/health.rs` | liveness and readiness probes |
| `src/handlers.rs` | Matrix event handlers (invites, messages) |
| `src/cluster.rs` | Kubernetes access shared by commands and alerts |
| `src/alerts.rs` | posts failed CI runs to the ops room |
| `src/commands/` | command registry, parsing and dispatch, one module per command |

## Adding a command

Create `src/commands/<name>.rs`, modelled on `ping.rs`:

```rust
use super::{Command, Ctx, Reply};

pub const COMMAND: Command = Command {
    name: "echo",
    usage: "!echo <text>",
    summary: "repeat the text back",
    run: |ctx| Box::pin(run(ctx)),
};

async fn run(ctx: Ctx) -> Reply {
    anyhow::ensure!(!ctx.args.is_empty(), "usage: `!echo <text>`");
    Ok(ctx.args)
}
```

Then declare it with `mod <name>;` in `src/commands/mod.rs` and add it to `ALL`; `!help` picks it up automatically.
The command gets the client, room, sender, raw arguments and cluster access through `Ctx`; an `Err` is logged and reported to the sender.
Each command runs in its own task, so slow ones don't hold up the bot.
