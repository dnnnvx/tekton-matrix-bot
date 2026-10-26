//! `!command` registry and dispatch.
//!
//! Adding a command: create a module exposing a `COMMAND`, then list it in [`ALL`].
//! `!help` is generated from the registry.

mod help;
mod ping;

use std::{future::Future, pin::Pin};

use matrix_sdk::{Client, Room, ruma::OwnedUserId};
use tracing::warn;

/// Every registered command, in `!help` order.
pub const ALL: &[Command] = &[help::COMMAND, ping::COMMAND];

pub struct Command {
    pub name: &'static str,
    /// Shown in `!help`, e.g. `!deploy <app> [--dry-run]`.
    pub usage: &'static str,
    pub summary: &'static str,
    pub run: fn(Ctx) -> BoxFuture<Reply>,
}

/// Markdown sent back as a reply. An error is reported to the sender and logged.
pub type Reply = anyhow::Result<String>;
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What a command gets to work with.
#[allow(dead_code, reason = "for commands that need more than their args")]
pub struct Ctx {
    pub client: Client,
    pub room: Room,
    pub sender: OwnedUserId,
    /// Text after the command name, trimmed. Commands parse it however they need.
    pub args: String,
}

/// Runs the command in `body`. `None` when `body` isn't a command, so the bot stays quiet.
pub async fn dispatch(
    body: &str,
    client: Client,
    room: Room,
    sender: OwnedUserId,
) -> Option<String> {
    let (name, args) = parse(body)?;
    let Some(command) = find(name) else {
        return Some(format!("unknown command `{name}`, try `!help`"));
    };
    let ctx = Ctx {
        client,
        room,
        sender,
        args: args.to_owned(),
    };
    match (command.run)(ctx).await {
        Ok(reply) => Some(reply),
        Err(e) => {
            warn!(command = command.name, "failed: {e:#}");
            Some(format!("`!{}` failed: {e}", command.name))
        }
    }
}

/// Splits `!name args…` into `("name", "args…")`. Anything else isn't a command.
fn parse(body: &str) -> Option<(&str, &str)> {
    let rest = body.trim().strip_prefix('!')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    valid.then_some((name, args.trim()))
}

fn find(name: &str) -> Option<&'static Command> {
    ALL.iter().find(|c| c.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(parse("!ping"), Some(("ping", "")));
        assert_eq!(
            parse("  !deploy  app  --dry-run "),
            Some(("deploy", "app  --dry-run"))
        );
        assert_eq!(parse("!help\nmore"), Some(("help", "more")));
    }

    #[test]
    fn ignores_non_commands() {
        for body in ["ping", "", "!", "! ping", "!!!", "wow!", "!?"] {
            assert_eq!(parse(body), None, "{body:?}");
        }
    }

    #[test]
    fn finds_case_insensitively() {
        assert_eq!(find("PING").map(|c| c.name), Some("ping"));
        assert!(find("nope").is_none());
    }

    #[test]
    fn registry_is_consistent() {
        for (i, c) in ALL.iter().enumerate() {
            assert!(
                parse(c.usage).is_some_and(|(name, _)| name == c.name),
                "{}",
                c.name
            );
            assert!(
                ALL[..i].iter().all(|o| o.name != c.name),
                "duplicate {}",
                c.name
            );
        }
    }
}
