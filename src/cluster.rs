//! What the ops commands and alerts need from the Kubernetes cluster.

use anyhow::Context;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api,
    api::{ApiResource, DynamicObject, GroupVersionKind, ListParams, LogParams},
};
use matrix_sdk::ruma::{OwnedRoomId, RoomId};

use crate::config::Config;

const CI_NAMESPACE: &str = "tekton-ci";
/// Enough to see why a step failed, small enough for a chat message.
const LOG_LINES: i64 = 30;

#[derive(Clone)]
pub struct Cluster {
    pub kube: kube::Client,
    pub dashboard_url: String,
    pub ops_room: OwnedRoomId,
}

impl Cluster {
    pub async fn connect(config: &Config) -> anyhow::Result<Self> {
        Ok(Self {
            kube: kube::Client::try_default()
                .await
                .context("no Kubernetes config")?,
            dashboard_url: config.tekton_dashboard_url.clone(),
            ops_room: config.ops_room.clone(),
        })
    }

    pub fn pipeline_runs(&self) -> Api<DynamicObject> {
        self.tekton("PipelineRun", "pipelineruns")
    }

    fn tekton(&self, kind: &str, plural: &str) -> Api<DynamicObject> {
        let gvk = GroupVersionKind::gvk("tekton.dev", "v1", kind);
        let resource = ApiResource::from_gvk_with_plural(&gvk, plural);
        Api::namespaced_with(self.kube.clone(), CI_NAMESPACE, &resource)
    }

    /// The last log lines of the step that failed the run.
    pub async fn failure_logs(&self, run: &DynamicObject) -> anyhow::Result<String> {
        let name = run.metadata.name.as_deref().unwrap_or_default();
        let selector = format!("tekton.dev/pipelineRun={name}");
        let task_runs = self
            .tekton("TaskRun", "taskruns")
            .list(&ListParams::default().labels(&selector))
            .await?;
        let (pod, container) = task_runs
            .items
            .iter()
            .filter(|task_run| run_failure(task_run).is_some())
            .find_map(failed_step)
            .context("no failed step found")?;
        let params = LogParams {
            container: Some(container.to_owned()),
            tail_lines: Some(LOG_LINES),
            ..LogParams::default()
        };
        let pods = Api::<Pod>::namespaced(self.kube.clone(), CI_NAMESPACE);
        Ok(pods.logs(pod, &params).await?)
    }

    /// Cluster state can be sensitive, so it's only shown to the ops room's members.
    pub fn ensure_ops_room(&self, room: &RoomId) -> anyhow::Result<()> {
        anyhow::ensure!(room == self.ops_room, "only available in the ops room");
        Ok(())
    }
}

pub struct Condition<'a> {
    pub status: &'a str,
    pub reason: &'a str,
    pub message: &'a str,
}

/// The `status.conditions` entry of the given type.
pub fn condition<'a>(object: &'a DynamicObject, kind: &str) -> Option<Condition<'a>> {
    let conditions = object.data.pointer("/status/conditions")?.as_array()?;
    let found = conditions.iter().find(|c| c["type"] == kind)?;
    Some(Condition {
        status: found["status"].as_str().unwrap_or_default(),
        reason: found["reason"].as_str().unwrap_or_default(),
        message: found["message"].as_str().unwrap_or_default(),
    })
}

/// A cancelled run was stopped on purpose, so it doesn't count as a failure.
pub fn run_failure<'a>(run: &'a DynamicObject) -> Option<Condition<'a>> {
    condition(run, "Succeeded").filter(|c| c.status == "False" && c.reason != "Cancelled")
}

/// A run with no condition yet hasn't been picked up, but it isn't finished either.
pub fn run_is_active(run: &DynamicObject) -> bool {
    condition(run, "Succeeded").is_none_or(|c| c.status == "Unknown")
}

/// The pod and container of a TaskRun's first step that exited non-zero.
fn failed_step(task_run: &DynamicObject) -> Option<(&str, &str)> {
    let status = &task_run.data["status"];
    let steps = status["steps"].as_array()?;
    let failed = steps.iter().find(|step| {
        step["terminated"]["exitCode"]
            .as_i64()
            .is_some_and(|code| code != 0)
    })?;
    Some((status["podName"].as_str()?, failed["container"].as_str()?))
}

/// Markdown link to the run in the Tekton dashboard.
pub fn run_link(dashboard_url: &str, run: &DynamicObject) -> String {
    let name = run.metadata.name.as_deref().unwrap_or_default();
    let url = dashboard_url.trim_end_matches('/');
    format!("[{name}]({url}/#/namespaces/{CI_NAMESPACE}/pipelineruns/{name})")
}

#[cfg(test)]
pub fn test_run(name: &str, status: &str, reason: &str) -> DynamicObject {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "tekton.dev/v1",
        "kind": "PipelineRun",
        "metadata": { "name": name, "uid": name },
        "status": {
            "startTime": format!("2026-10-30T10:00:0{}Z", name.len() % 10),
            "completionTime": "2026-10-30T11:00:00Z",
            "conditions": [{
                "type": "Succeeded",
                "status": status,
                "reason": reason,
                "message": "Tasks Completed: 2 (Failed: 1, Cancelled 0), Skipped: 2",
            }],
        },
    }))
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_runs() {
        let failed = test_run("a", "False", "Failed");
        assert!(run_failure(&failed).is_some() && !run_is_active(&failed));

        let timed_out = test_run("a", "False", "PipelineRunTimeout");
        assert!(run_failure(&timed_out).is_some());

        let cancelled = test_run("a", "False", "Cancelled");
        assert!(run_failure(&cancelled).is_none() && !run_is_active(&cancelled));

        let running = test_run("a", "Unknown", "Running");
        assert!(run_failure(&running).is_none() && run_is_active(&running));

        let succeeded = test_run("a", "True", "Succeeded");
        assert!(run_failure(&succeeded).is_none() && !run_is_active(&succeeded));

        let mut new = test_run("a", "Unknown", "Running");
        new.data = serde_json::json!({});
        assert!(run_failure(&new).is_none() && run_is_active(&new));
    }

    #[test]
    fn finds_the_failed_step() {
        let task_run: DynamicObject = serde_json::from_value(serde_json::json!({
            "apiVersion": "tekton.dev/v1",
            "kind": "TaskRun",
            "metadata": { "name": "build-1-test" },
            "status": {
                "podName": "build-1-test-pod",
                "steps": [
                    { "container": "step-fetch", "terminated": { "exitCode": 0 } },
                    { "container": "step-test", "terminated": { "exitCode": 101 } },
                    { "container": "step-push", "terminated": { "exitCode": 1 } },
                ],
            },
        }))
        .unwrap();
        assert_eq!(
            failed_step(&task_run),
            Some(("build-1-test-pod", "step-test"))
        );

        let mut running = task_run;
        running.data["status"]["steps"] =
            serde_json::json!([{ "container": "step-fetch", "running": {} }]);
        assert_eq!(failed_step(&running), None);
    }

    #[test]
    fn parses_hash_room_ids() {
        let room: OwnedRoomId = "!hcNOO-kFitB4fJ15jq8k5Ju5H56mGzLYXIIdbuhJvXM"
            .parse()
            .unwrap();
        assert_eq!(
            room.as_str(),
            "!hcNOO-kFitB4fJ15jq8k5Ju5H56mGzLYXIIdbuhJvXM"
        );
    }

    #[test]
    fn links_to_the_dashboard() {
        assert_eq!(
            run_link(
                "https://tekton.example/",
                &test_run("build-1", "True", "Succeeded")
            ),
            "[build-1](https://tekton.example/#/namespaces/tekton-ci/pipelineruns/build-1)"
        );
    }
}
