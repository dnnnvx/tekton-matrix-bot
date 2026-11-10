use std::fmt::Write;

use k8s_openapi::api::core::v1::ConfigMap;
use kube::Api;
use kube::api::{DynamicObject, GroupVersionKind};

use super::{Command, Ctx, Reply};
use crate::cluster::{Cluster, condition};
use crate::markdown::{code_block, head};

pub const COMMAND: Command = Command {
    name: "tofu",
    usage: "!tofu",
    summary: "pending tofu plan, and the command to apply it",
    run: |ctx| Box::pin(run(ctx)),
};

const NAMESPACE: &str = "flux-system";
const TERRAFORM: &str = "production";
const PLAN_CONFIGMAP: &str = "tfplan-default-production";
/// An event caps at 65 KB and holds the text twice, plain and as escaped HTML.
const MAX_PLAN_BYTES: usize = 20_000;

async fn run(ctx: Ctx) -> Reply {
    ctx.cluster.ensure_ops_room(ctx.room.room_id())?;
    Plan::fetch(&ctx.cluster).await?.render()
}

/// The Terraform object, and the ConfigMap holding its readable plan if there is one.
pub struct Plan {
    terraform: DynamicObject,
    stored: Option<ConfigMap>,
}

impl Plan {
    pub async fn fetch(cluster: &Cluster) -> anyhow::Result<Self> {
        let gvk = GroupVersionKind::gvk("infra.contrib.fluxcd.io", "v1alpha2", "Terraform");
        let terraforms = cluster.custom(&gvk, "terraforms", NAMESPACE);
        let terraform = terraforms.get(TERRAFORM).await?;

        let stored = Api::<ConfigMap>::namespaced(cluster.kube.clone(), NAMESPACE)
            .get_opt(PLAN_CONFIGMAP)
            .await?;
        Ok(Self { terraform, stored })
    }

    /// A non-empty field of the Terraform object's `status.plan`.
    fn plan_status(&self, field: &str) -> Option<&str> {
        let value = self.terraform.data["status"]["plan"][field].as_str();
        value.filter(|v| !v.is_empty())
    }

    fn pending(&self) -> Option<&str> {
        self.plan_status("pending")
    }

    /// The revision of the stored plan text.
    fn saved(&self) -> Option<&str> {
        let annotations = self.stored.as_ref()?.metadata.annotations.as_ref()?;
        annotations.get("savedPlan").map(String::as_str)
    }

    /// The pending plan, once its readable text is stored and [`Plan::render`] can show it.
    pub fn stored_pending(&self) -> Option<&str> {
        self.pending()
            .filter(|pending| self.saved() == Some(*pending))
    }

    pub fn render(&self) -> Reply {
        let mut reply = format!("**Tofu** `{TERRAFORM}`");
        if let Some(ready) = condition(&self.terraform, "Ready") {
            write!(reply, ": ready `{}` (`{}`)", ready.status, ready.reason)?;
        }
        if let Some(applied) = self.plan_status("lastApplied") {
            write!(reply, "\n\nLast applied: `{applied}`")?;
        }
        let Some(pending) = self.pending() else {
            reply.push_str("\n\nNo pending plan.");
            return Ok(reply);
        };
        write!(reply, "\n\n**Pending:** `{pending}`")?;

        let saved = self.saved();
        let text = self
            .stored
            .as_ref()
            .and_then(|c| c.data.as_ref()?.get("tfplan"));
        match text {
            // A stored plan from another revision isn't what the command below would apply.
            Some(text) if saved == Some(pending) => {
                write!(reply, "\n\n{}", code_block(head(text, MAX_PLAN_BYTES)))?;
                if text.len() > MAX_PLAN_BYTES {
                    reply.push_str("\n\nPlan cut short, see `just tofu-plans` for the rest.");
                }
            }
            Some(_) => write!(
                reply,
                "\n\n⚠️ The stored plan is `{}`, not the pending one, so it isn't shown.",
                saved.unwrap_or("unknown")
            )?,
            None => reply.push_str("\n\n⚠️ The plan isn't stored yet, so it isn't shown."),
        }

        let patch = format!(r#"{{"spec":{{"approvePlan":"{pending}"}}}}"#);
        let apply = format!(
            "kubectl --context homelab -n {NAMESPACE} patch terraform {TERRAFORM} \
             --type merge --field-manager tofu-approve -p '{patch}'"
        );
        write!(reply, "\n\nApply with:\n\n{}", code_block(&apply))?;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn plan(pending: &str, stored: Option<ConfigMap>) -> Plan {
        let terraform = serde_json::from_value(json!({
            "apiVersion": "infra.contrib.fluxcd.io/v1alpha2",
            "kind": "Terraform",
            "metadata": { "name": TERRAFORM },
            "status": {
                "plan": { "pending": pending, "lastApplied": "plan-main-old" },
                "conditions": [{ "type": "Ready", "status": "Unknown", "reason": "Planned" }],
            },
        }))
        .unwrap();
        Plan { terraform, stored }
    }

    fn stored(saved: &str, text: &str) -> Option<ConfigMap> {
        let config_map = serde_json::from_value(json!({
            "metadata": { "name": PLAN_CONFIGMAP, "annotations": { "savedPlan": saved } },
            "data": { "tfplan": text },
        }));
        Some(config_map.unwrap())
    }

    #[test]
    fn shows_the_pending_plan_and_how_to_apply_it() {
        let stored = stored("plan-main-new", "Plan: 1 to add");
        let reply = plan("plan-main-new", stored).render().unwrap();
        assert!(reply.contains("ready `Unknown` (`Planned`)"));
        assert!(reply.contains("Last applied: `plan-main-old`"));
        assert!(reply.contains("**Pending:** `plan-main-new`"));
        assert!(reply.contains("```\nPlan: 1 to add\n```"));
        assert!(reply.contains(
            r#"patch terraform production --type merge --field-manager tofu-approve -p '{"spec":{"approvePlan":"plan-main-new"}}'"#
        ));
    }

    #[test]
    fn nothing_to_apply_without_a_pending_plan() {
        let reply = plan("", stored("plan-main-old", "old plan"))
            .render()
            .unwrap();
        assert!(reply.contains("No pending plan."));
        assert!(!reply.contains("old plan") && !reply.contains("kubectl"));
    }

    #[test]
    fn hides_a_stored_plan_of_another_revision() {
        let stored = stored("plan-main-other", "other plan");
        let reply = plan("plan-main-new", stored).render().unwrap();
        assert!(reply.contains("⚠️ The stored plan is `plan-main-other`"));
        assert!(!reply.contains("other plan"));

        let reply = plan("plan-main-new", None).render().unwrap();
        assert!(reply.contains("⚠️ The plan isn't stored yet"));
    }

    #[test]
    fn pending_counts_as_stored_once_its_text_is() {
        let new = plan("plan-main-new", stored("plan-main-new", "new plan"));
        assert_eq!(new.stored_pending(), Some("plan-main-new"));
        let other = plan("plan-main-new", stored("plan-main-other", "other plan"));
        assert_eq!(other.stored_pending(), None);
        assert_eq!(plan("plan-main-new", None).stored_pending(), None);
        let applied = plan("", stored("plan-main-new", "new plan"));
        assert_eq!(applied.stored_pending(), None);
    }

    #[test]
    fn cuts_long_plans_on_a_character_boundary() {
        let stored = stored("plan-main-new", &"é".repeat(MAX_PLAN_BYTES));
        let reply = plan("plan-main-new", stored).render().unwrap();
        assert!(reply.contains("Plan cut short"));
        assert!(reply.len() < MAX_PLAN_BYTES + 1000);
    }
}
