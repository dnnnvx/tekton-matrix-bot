use kube::api::{DynamicObject, ListParams};

use super::{Command, Ctx, Reply};
use crate::cluster::{run_failure, run_is_active, run_link};

pub const COMMAND: Command = Command {
    name: "ci",
    usage: "!ci [list]",
    summary: "running builds and recent failures",
    run: |ctx| Box::pin(run(ctx)),
};

const RECENT_FAILURES: usize = 5;

async fn run(ctx: Ctx) -> Reply {
    ctx.cluster.ensure_ops_room(ctx.room.room_id())?;
    anyhow::ensure!(
        matches!(ctx.args.as_str(), "" | "list"),
        "usage: `!ci [list]`"
    );
    let runs = ctx
        .cluster
        .pipeline_runs()
        .list(&ListParams::default())
        .await?;
    Ok(render(&ctx.cluster.dashboard_url, runs.items))
}

fn render(dashboard_url: &str, mut runs: Vec<DynamicObject>) -> String {
    // RFC 3339 UTC timestamps sort chronologically as text.
    runs.sort_by_key(|run| std::cmp::Reverse(time(run, "startTime").to_owned()));

    let running = runs.iter().filter(|run| run_is_active(run)).map(|run| {
        let link = run_link(dashboard_url, run);
        format!("- {link}, started {}", time(run, "startTime"))
    });
    let failures = runs
        .iter()
        .filter_map(|run| {
            let reason = run_failure(run)?.reason;
            let link = run_link(dashboard_url, run);
            Some(format!(
                "- {link} (`{reason}`), {}",
                time(run, "completionTime")
            ))
        })
        .take(RECENT_FAILURES);

    format!(
        "**Running**\n{}\n\n**Recent failures**\n{}",
        bullets(running),
        bullets(failures)
    )
}

fn bullets(lines: impl Iterator<Item = String>) -> String {
    let joined = lines.collect::<Vec<_>>().join("\n");
    if joined.is_empty() {
        "- nothing".to_owned()
    } else {
        joined
    }
}

fn time<'a>(run: &'a DynamicObject, field: &str) -> &'a str {
    run.data["status"][field].as_str().unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::test_run;

    #[test]
    fn lists_running_and_recent_failures() {
        let mut runs = vec![
            test_run("done", "True", "Succeeded"),
            test_run("building", "Unknown", "Running"),
            test_run("stopped", "False", "Cancelled"),
        ];
        // Name lengths set the start times: the longest name is the newest.
        let names = ["f", "ff", "fff", "ffff", "fffff", "ffffff"];
        runs.extend(names.iter().map(|n| test_run(n, "False", "Failed")));

        let reply = render("https://t", runs);
        let link = |n: &str| format!("[{n}](https://t/#/namespaces/tekton-ci/pipelineruns/{n})");

        assert!(reply.contains(&format!("- {}, started", link("building"))));
        assert!(reply.contains(&format!("- {} (`Failed`)", link("ffffff"))));
        assert!(reply.contains(&format!("- {} (`Failed`)", link("ff"))));
        assert!(!reply.contains(&link("f")), "only the 5 newest failures");
        assert!(!reply.contains("done") && !reply.contains("stopped"));
    }

    #[test]
    fn says_when_there_is_nothing() {
        assert_eq!(
            render("https://t", vec![]),
            "**Running**\n- nothing\n\n**Recent failures**\n- nothing"
        );
    }
}
