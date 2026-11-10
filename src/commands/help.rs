use std::fmt::Write as _;

use super::{ALL, Command, Ctx, Reply};

pub const COMMAND: Command = Command {
    name: "help",
    usage: "!help",
    summary: "this message",
    ops_only: false,
    run: |ctx| Box::pin(run(ctx)),
};

async fn run(_: Ctx) -> Reply {
    let mut reply = String::from("**Commands**");
    for command in ALL {
        write!(reply, "\n- `{}`: {}", command.usage, command.summary)?;
        if command.ops_only {
            reply.push_str(" (ops room only)");
        }
    }
    Ok(reply)
}
