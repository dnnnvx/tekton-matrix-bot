use std::fmt::Write;

use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    Api,
    api::{ApiResource, DynamicObject, GroupVersionKind},
};

use super::{Command, Ctx, Reply};
use crate::cluster::condition;

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
    let kube = &ctx.cluster.kube;

    let gvk = GroupVersionKind::gvk("infra.contrib.fluxcd.io", "v1alpha2", "Terraform");
    let resource = ApiResource::from_gvk_with_plural(&gvk, "terraforms");
    let terraforms = Api::<DynamicObject>::namespaced_with(kube.clone(), NAMESPACE, &resource);
    let terraform = terraforms.get(TERRAFORM).await?;

    let stored = Api::<ConfigMap>::namespaced(kube.clone(), NAMESPACE)
        .get_opt(PLAN_CONFIGMAP)
        .await?;
    render(&terraform, stored.as_ref())
}

fn render(terraform: &DynamicObject, stored: Option<&ConfigMap>) -> Reply {
    let mut reply = format!("**Tofu** `{TERRAFORM}`");
    if let Some(ready) = condition(terraform, "Ready") {
        write!(reply, ": ready `{}` (`{}`)", ready.status, ready.reason)?;
    }
    let plan_field = |field: &str| terraform.data["status"]["plan"][field].as_str();
    if let Some(applied) = plan_field("lastApplied").filter(|p| !p.is_empty()) {
        write!(reply, "\n\nLast applied: `{applied}`")?;
    }
    let Some(pending) = plan_field("pending").filter(|p| !p.is_empty()) else {
        reply.push_str("\n\nNo pending plan.");
        return Ok(reply);
    };
    write!(reply, "\n\n**Pending:** `{pending}`")?;

    let saved = stored
        .and_then(|c| c.metadata.annotations.as_ref()?.get("savedPlan"))
        .map(String::as_str);
    let text = stored.and_then(|c| c.data.as_ref()?.get("tfplan"));
    match text {
        // A stored plan from another revision isn't what the command below would apply.
        Some(text) if saved == Some(pending) => {
            write!(reply, "\n\n{}", code_block(cut(text)))?;
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

fn cut(text: &str) -> &str {
    &text[..text.floor_char_boundary(MAX_PLAN_BYTES)]
}

/// A fence longer than any backtick run in the text, which can't close it early then.
fn code_block(text: &str) -> String {
    let longest = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}\n{text}\n{fence}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn terraform(pending: &str) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "infra.contrib.fluxcd.io/v1alpha2",
            "kind": "Terraform",
            "metadata": { "name": TERRAFORM },
            "status": {
                "plan": { "pending": pending, "lastApplied": "plan-main-old" },
                "conditions": [{ "type": "Ready", "status": "Unknown", "reason": "Planned" }],
            },
        }))
        .unwrap()
    }

    fn stored(saved: &str, plan: &str) -> ConfigMap {
        serde_json::from_value(json!({
            "metadata": { "name": PLAN_CONFIGMAP, "annotations": { "savedPlan": saved } },
            "data": { "tfplan": plan },
        }))
        .unwrap()
    }

    #[test]
    fn shows_the_pending_plan_and_how_to_apply_it() {
        let plan = stored("plan-main-new", "Plan: 1 to add");
        let reply = render(&terraform("plan-main-new"), Some(&plan)).unwrap();
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
        let plan = stored("plan-main-old", "old plan");
        let reply = render(&terraform(""), Some(&plan)).unwrap();
        assert!(reply.contains("No pending plan."));
        assert!(!reply.contains("old plan") && !reply.contains("kubectl"));
    }

    #[test]
    fn hides_a_stored_plan_of_another_revision() {
        let plan = stored("plan-main-other", "other plan");
        let reply = render(&terraform("plan-main-new"), Some(&plan)).unwrap();
        assert!(reply.contains("⚠️ The stored plan is `plan-main-other`"));
        assert!(!reply.contains("other plan"));

        let reply = render(&terraform("plan-main-new"), None).unwrap();
        assert!(reply.contains("⚠️ The plan isn't stored yet"));
    }

    #[test]
    fn cuts_long_plans_on_a_character_boundary() {
        let plan = stored("plan-main-new", &"é".repeat(MAX_PLAN_BYTES));
        let reply = render(&terraform("plan-main-new"), Some(&plan)).unwrap();
        assert!(reply.contains("Plan cut short"));
        assert!(reply.len() < MAX_PLAN_BYTES + 1000);
    }

    #[test]
    fn fence_outgrows_backticks_in_the_text() {
        assert_eq!(code_block("a"), "```\na\n```");
        assert_eq!(code_block("a ```` b"), "`````\na ```` b\n`````");
    }
}
