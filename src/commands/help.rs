use std::fmt::Write as _;

use super::{ALL, Command, Ctx, Reply};

pub const COMMAND: Command = Command {
    name: "help",
    usage: "!help",
    summary: "this message",
    run: |ctx| Box::pin(run(ctx)),
};

async fn run(_: Ctx) -> Reply {
    let mut reply = String::from("**Commands**");
    for c in ALL {
        write!(reply, "\n- `{}`: {}", c.usage, c.summary)?;
    }
    Ok(reply)
}
