//! Posts CI runs and pending tofu plans to the ops room.

use std::collections::HashMap;
use std::pin::pin;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use kube::api::DynamicObject;
use kube::runtime::{WatchStreamExt, watcher};
use matrix_sdk::Client;
use matrix_sdk::ruma::events::relation::Reply;
use matrix_sdk::ruma::events::room::message::{
    Relation, ReplacementMetadata, RoomMessageEventContent,
};
use matrix_sdk::ruma::{OwnedEventId, RoomId};
use tracing::{error, warn};

use crate::cluster::{Cluster, RunState, run_link, run_name, run_state};
use crate::commands::tofu::Plan;
use crate::markdown::{code_block, tail};

/// Enough for the error, short enough to read in a chat. An event caps at 65 KB.
const MAX_LOG_BYTES: usize = 4_000;

/// Watches forever. The watcher re-lists and retries by itself after an error.
/// A run's start is posted, then edited into its result when it ends; a failure
/// also gets a reply with the logs.
pub async fn watch_ci(client: Client, cluster: Cluster, gc: Duration) {
    let events = watcher(cluster.pipeline_runs(), watcher::Config::default()).default_backoff();
    let mut events = pin!(events);
    let mut running = Running::default();
    while let Some(event) = events.next().await {
        let event = match event {
            Ok(event) => event,
            Err(e) => {
                warn!("watching PipelineRuns failed: {e}");
                continue;
            }
        };
        running.forget_older_than(gc);
        match running.change(&event) {
            None => {}
            Some(Change::Started(run)) => {
                let text = message(&cluster.dashboard_url, run, None);
                let sent = post(
                    &client,
                    &cluster.ops_room,
                    RoomMessageEventContent::text_markdown(text),
                )
                .await;
                running.track(run, sent);
            }
            Some(Change::Ended(run, started)) => {
                let (url, room) = (&cluster.dashboard_url, &cluster.ops_room);
                let failure = match run_state(run) {
                    RunState::Failed(_) => {
                        let logs = cluster.failure_logs(run).await.inspect_err(|e| {
                            warn!(run = run_name(run), "no failure logs: {e:#}");
                        });
                        Some(message(url, run, logs.ok().as_deref()))
                    }
                    _ => None,
                };
                let Some(started) = started else {
                    // The start wasn't posted when sending failed: the result gets a message of its own.
                    let text = failure.unwrap_or_else(|| message(url, run, None));
                    post(&client, room, RoomMessageEventContent::text_markdown(text)).await;
                    continue;
                };
                let edit = RoomMessageEventContent::text_markdown(message(url, run, None))
                    .make_replacement(ReplacementMetadata::new(started.clone(), None));
                post(&client, room, edit).await;
                // Clients usually don't notify on edits, but do on a new message.
                if let Some(failure) = failure {
                    let mut reply = RoomMessageEventContent::text_markdown(failure);
                    reply.relates_to = Some(Relation::Reply(Reply::with_event_id(started)));
                    post(&client, room, reply).await;
                }
            }
        }
    }
}

/// Checks forever, the first time right away. A pending plan is posted at every
/// check, as a reminder of the drift, once its text is stored.
pub async fn poll_tofu(client: Client, cluster: Cluster, every: Duration) {
    let mut ticks = tokio::time::interval(every);
    loop {
        ticks.tick().await;
        let plan = match Plan::fetch(&cluster).await {
            Ok(plan) => plan,
            Err(e) => {
                warn!("checking the tofu plan failed: {e:#}");
                continue;
            }
        };
        if plan.stored_pending().is_none() {
            continue;
        }
        let text = match plan.render() {
            Ok(text) => text,
            Err(e) => {
                error!("rendering the tofu plan failed: {e:#}");
                continue;
            }
        };
        let content = RoomMessageEventContent::text_markdown(format!("🟠 Drift\n\n{text}"));
        post(&client, &cluster.ops_room, content).await;
    }
}

/// Sends to the ops room. The event ID is what a later edit refers to.
async fn post(
    client: &Client,
    room_id: &RoomId,
    content: RoomMessageEventContent,
) -> Option<OwnedEventId> {
    let Some(room) = client.get_room(room_id) else {
        warn!(room = %room_id, "not in the ops room, dropping alert");
        return None;
    };
    match room.send(content).await {
        Ok(sent) => Some(sent.response.event_id),
        Err(e) => {
            error!(room = %room_id, "send failed: {e}");
            None
        }
    }
}

/// The runs whose start was posted and whose end wasn't yet, by UID.
/// shortcut: a run that starts and ends while the bot is down or disconnected is never posted.
#[derive(Default)]
struct Running(HashMap<String, Tracked>);

struct Tracked {
    /// `None` when posting the start failed.
    started: Option<OwnedEventId>,
    since: Instant,
}

enum Change<'a> {
    Started(&'a DynamicObject),
    /// With the start message to edit.
    Ended(&'a DynamicObject, Option<OwnedEventId>),
}

impl Running {
    fn change<'a>(&mut self, event: &'a watcher::Event<DynamicObject>) -> Option<Change<'a>> {
        let run = match event {
            watcher::Event::Init | watcher::Event::InitDone => return None,
            watcher::Event::Delete(run) => {
                self.0.remove(run.metadata.uid.as_deref()?);
                return None;
            }
            // Runs already active when the bot starts are posted as started too.
            watcher::Event::InitApply(run) | watcher::Event::Apply(run) => run,
        };
        let uid = run.metadata.uid.as_deref()?;
        if matches!(run_state(run), RunState::Running) {
            (!self.0.contains_key(uid)).then_some(Change::Started(run))
        } else {
            // An untracked finished run was posted already, or ended before the bot saw it start.
            let tracked = self.0.remove(uid)?;
            Some(Change::Ended(run, tracked.started))
        }
    }

    fn track(&mut self, run: &DynamicObject, started: Option<OwnedEventId>) {
        if let Some(uid) = run.metadata.uid.clone() {
            let since = Instant::now();
            self.0.insert(uid, Tracked { started, since });
        }
    }

    /// No run lasts this long: its end was missed, e.g. it was deleted while the watch was down.
    fn forget_older_than(&mut self, gc: Duration) {
        self.0.retain(|_, tracked| tracked.since.elapsed() < gc);
    }
}

fn message(dashboard_url: &str, run: &DynamicObject, logs: Option<&str>) -> String {
    let link = run_link(dashboard_url, run);
    let failed = match run_state(run) {
        RunState::Running => return format!("🟡 **CI running:** {link}"),
        RunState::Succeeded => return format!("✅ **CI succeeded:** {link}"),
        RunState::Cancelled(reason) => return format!("⏹️ **CI cancelled:** {link} (`{reason}`)"),
        RunState::Failed(failed) => failed,
    };
    let mut text = format!(
        "🔴 **CI failed:** {link} (`{}`) {}",
        failed.reason, failed.message
    );
    if let Some(logs) = logs {
        // The end of the logs is where the error is.
        text.push_str("\n\n");
        text.push_str(&code_block(tail(logs, MAX_LOG_BYTES).trim_end()));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::test_run;

    fn event_id() -> OwnedEventId {
        "$start:example.org".parse().unwrap()
    }

    #[test]
    fn posts_the_start_then_the_end_once() {
        let mut running = Running::default();
        let started = || test_run("a", "Unknown", "Running");
        let done = || test_run("a", "True", "Succeeded");

        // Before the bot started: a finished run stays quiet, an active one is posted.
        let old = watcher::Event::InitApply(test_run("old", "False", "Failed"));
        assert!(running.change(&old).is_none());
        let event = watcher::Event::InitApply(started());
        let Some(Change::Started(run)) = running.change(&event) else {
            panic!("start not posted");
        };
        running.track(run, Some(event_id()));
        assert!(running.change(&watcher::Event::InitDone).is_none());

        assert!(running.change(&watcher::Event::Apply(started())).is_none());
        let event = watcher::Event::Apply(done());
        let Some(Change::Ended(_, Some(id))) = running.change(&event) else {
            panic!("end not posted as an edit");
        };
        assert_eq!(id, event_id());
        assert!(running.change(&watcher::Event::Apply(done())).is_none());
        assert!(running.0.is_empty(), "ended runs are forgotten");
    }

    #[test]
    fn forgets_deleted_and_stale_runs() {
        let mut running = Running::default();
        let run = test_run("a", "Unknown", "Running");
        running.track(&run, None);
        assert!(
            running
                .change(&watcher::Event::Delete(run.clone()))
                .is_none()
        );
        assert!(running.0.is_empty());

        running.track(&run, None);
        running.forget_older_than(Duration::from_secs(3600));
        assert_eq!(running.0.len(), 1);
        running.forget_older_than(Duration::ZERO);
        assert!(running.0.is_empty());
    }

    #[test]
    fn formats_each_state() {
        let link = |n: &str| format!("[{n}](https://t/#/namespaces/tekton-ci/pipelineruns/{n})");
        let text =
            |status, reason, logs| message("https://t", &test_run("b", status, reason), logs);
        assert_eq!(
            text("Unknown", "Running", None),
            format!("🟡 **CI running:** {}", link("b"))
        );
        assert_eq!(
            text("True", "Succeeded", None),
            format!("✅ **CI succeeded:** {}", link("b"))
        );
        assert_eq!(
            text("False", "Cancelled", None),
            format!("⏹️ **CI cancelled:** {} (`Cancelled`)", link("b"))
        );
        assert_eq!(
            text("False", "Failed", Some("compiling\nerror[E0308]\n")),
            format!(
                "🔴 **CI failed:** {} (`Failed`) Tasks Completed: 2 (Failed: 1, Cancelled 0), Skipped: 2\
                 \n\n```\ncompiling\nerror[E0308]\n```",
                link("b")
            )
        );
    }

    #[test]
    fn keeps_the_end_of_long_logs() {
        let logs = format!("{}the error", "é".repeat(MAX_LOG_BYTES));
        let text = message("https://t", &test_run("b", "False", "Failed"), Some(&logs));
        assert!(text.ends_with("the error\n```"));
        assert!(text.len() < MAX_LOG_BYTES + 500);
    }
}
