//! Posts failed CI runs to the ops room.

use std::{collections::HashSet, pin::pin};

use futures_util::StreamExt;
use kube::{
    api::DynamicObject,
    runtime::{WatchStreamExt, watcher},
};
use matrix_sdk::{Client, ruma::events::room::message::RoomMessageEventContent};
use tracing::{error, warn};

use crate::cluster::{Cluster, run_failure, run_link};

/// Watches forever. The watcher re-lists and retries by itself after an error.
pub async fn watch_ci(client: Client, cluster: Cluster) {
    let events = watcher(cluster.pipeline_runs(), watcher::Config::default()).default_backoff();
    let mut events = pin!(events);
    let mut failures = Failures::default();
    while let Some(event) = events.next().await {
        let event = match event {
            Ok(event) => event,
            Err(e) => {
                warn!("watching PipelineRuns failed: {e}");
                continue;
            }
        };
        let Some(run) = failures.new_failure(&event) else {
            continue;
        };
        let Some(room) = client.get_room(&cluster.ops_room) else {
            warn!(room = %cluster.ops_room, "not in the ops room, dropping CI alert");
            continue;
        };
        let content = RoomMessageEventContent::text_markdown(message(&cluster.dashboard_url, run));
        if let Err(e) = room.send(content).await {
            error!(room = %room.room_id(), "send failed: {e}");
        }
    }
}

/// Which failures were already seen: a failed run keeps getting updates, and a
/// reconnecting watcher lists every run again.
#[derive(Default)]
struct Failures {
    seen: HashSet<String>,
    /// Failures from the first listing predate this process: remembered, not alerted.
    /// shortcut: a run failing while the bot is down is never alerted, persist `seen` if that matters.
    listed: bool,
}

impl Failures {
    fn new_failure<'a>(
        &mut self,
        event: &'a watcher::Event<DynamicObject>,
    ) -> Option<&'a DynamicObject> {
        let run = match event {
            watcher::Event::Init | watcher::Event::Delete(_) => return None,
            watcher::Event::InitDone => {
                self.listed = true;
                return None;
            }
            watcher::Event::InitApply(run) | watcher::Event::Apply(run) => run,
        };
        run_failure(run)?;
        let new = self.seen.insert(run.metadata.uid.clone()?);
        (new && self.listed).then_some(run)
    }
}

fn message(dashboard_url: &str, run: &DynamicObject) -> String {
    let failure = run_failure(run);
    let (reason, details) = failure.map_or(("", ""), |c| (c.reason, c.message));
    format!(
        "🔴 **CI failed:** {} (`{reason}`) {details}",
        run_link(dashboard_url, run)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::test_run;

    fn is_new(failures: &mut Failures, event: watcher::Event<DynamicObject>) -> bool {
        failures.new_failure(&event).is_some()
    }

    #[test]
    fn alerts_each_new_failure_once() {
        let mut failures = Failures::default();
        let old = || test_run("old", "False", "Failed");
        let new = || test_run("new", "False", "Failed");

        assert!(!is_new(&mut failures, watcher::Event::Init));
        assert!(!is_new(&mut failures, watcher::Event::InitApply(old())));
        assert!(!is_new(&mut failures, watcher::Event::InitDone));

        let running = test_run("new", "Unknown", "Running");
        assert!(!is_new(&mut failures, watcher::Event::Apply(running)));
        assert!(is_new(&mut failures, watcher::Event::Apply(new())));
        assert!(!is_new(&mut failures, watcher::Event::Apply(new())));

        // After a reconnect: what was seen stays quiet, what failed meanwhile is alerted.
        let missed = test_run("missed", "False", "Failed");
        assert!(!is_new(&mut failures, watcher::Event::Init));
        assert!(!is_new(&mut failures, watcher::Event::InitApply(old())));
        assert!(!is_new(&mut failures, watcher::Event::InitApply(new())));
        assert!(is_new(&mut failures, watcher::Event::InitApply(missed)));
    }

    #[test]
    fn formats_the_alert() {
        assert_eq!(
            message("https://t", &test_run("b", "False", "Failed")),
            "🔴 **CI failed:** [b](https://t/#/namespaces/tekton-ci/pipelineruns/b) (`Failed`) \
             Tasks Completed: 2 (Failed: 1, Cancelled 0), Skipped: 2"
        );
    }
}
