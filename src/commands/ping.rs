use super::{Command, Ctx, Reply};

pub const COMMAND: Command = Command {
    name: "ping",
    usage: "!ping",
    summary: "check the bot is alive",
    run: |ctx| Box::pin(run(ctx)),
};

async fn run(_: Ctx) -> Reply {
    Ok("pong".to_owned())
}
