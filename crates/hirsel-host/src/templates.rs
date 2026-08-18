use std::path::PathBuf;

mod bind;
mod spec;
mod store;
mod views;

pub use bind::bind_spec;
pub use spec::validate;
pub use store::{TemplateStore, TemplateSummary};
pub use views::ViewManager;

pub fn bundled_templates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../templates")
}

pub fn bundled_docs_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/hirsel-config.md")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hirsel_proto::HostToClient;
    use serde_json::json;
    use tokio::sync::broadcast;

    use super::{TemplateStore, ViewManager, bind_spec, bundled_templates_dir, validate};
    use crate::BroadcastLog;

    #[test]
    fn binds_typed_values_interpolation_and_each_blocks() {
        let spec = json!({
            "type": "stack",
            "gap": "sm",
            "children": [
                { "type": "text", "text": "{{count}} checks" },
                { "{{#each checks}}": {
                    "type": "text",
                    "text": "{{label}}",
                    "tone": "{{tone}}"
                }}
            ]
        });
        let resolved = bind_spec(
            &spec,
            &json!({
                "count": 2,
                "checks": [
                    { "label": "Build", "tone": "success" },
                    { "label": "Review", "tone": "muted" }
                ]
            }),
        )
        .unwrap();
        assert_eq!(resolved["children"][0]["text"], "2 checks");
        assert_eq!(resolved["children"][1]["text"], "Build");
        assert_eq!(resolved["children"][2]["tone"], "muted");
        validate(&resolved).unwrap();
    }

    #[test]
    fn validation_rejects_unknown_components_and_properties() {
        let error = validate(&json!({ "type": "marquee", "text": "no" }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown component type `marquee`"));

        let error = validate(&json!({ "type": "text", "text": "ok", "flash": true }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown property `flash`"));
    }

    #[tokio::test]
    async fn template_resolve_reloads_edits_without_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.json");
        tokio::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "id": "status",
                "title": "Status",
                "params_schema": { "message": "string" },
                "spec": { "type": "text", "text": "{{message}}" }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let store = TemplateStore::load(dir.path().to_path_buf()).await.unwrap();
        let first = store
            .resolve("status", json!({ "message": "calm" }))
            .await
            .unwrap();
        assert_eq!(first["text"], "calm");

        tokio::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "id": "status",
                "title": "Status",
                "params_schema": { "message": "string" },
                "spec": { "type": "text", "text": "Now: {{message}}" }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let second = store
            .resolve("status", json!({ "message": "ready" }))
            .await
            .unwrap();
        assert_eq!(second["text"], "Now: ready");
    }

    #[tokio::test]
    async fn seed_templates_resolve_and_validate() {
        let store = TemplateStore::load(bundled_templates_dir()).await.unwrap();
        let cases = BTreeMap::from([
            (
                "decision",
                json!({
                    "title": "Choose a release window",
                    "context": "Two safe windows remain.",
                    "question": "Which one should I use?",
                    "choices": [
                        { "label": "Tonight", "value": "tonight", "description": "Lower traffic." },
                        { "label": "Tomorrow", "value": "tomorrow", "description": "More observers." }
                    ]
                }),
            ),
            (
                "pr-summary",
                json!({
                    "title": "Keep reconnect snapshots stable",
                    "branch": "feat/reconnect",
                    "files": 8,
                    "tests_ok": true,
                    "tests_label": "Tests passing",
                    "tests_state": "success",
                    "checks": [
                        { "label": "Build", "checked": true, "detail": "Workspace" },
                        { "label": "Review", "checked": false, "detail": "Awaiting owner" }
                    ]
                }),
            ),
            (
                "status-digest",
                json!({
                    "title": "Workstream status",
                    "updated": "just now",
                    "workstreams": [
                        { "name": "Host", "state": "success", "detail": "Steady." },
                        { "name": "Client", "state": "running", "detail": "In progress." }
                    ]
                }),
            ),
            (
                "table-report",
                json!({
                    "title": "Checks",
                    "summary": "All required checks reported.",
                    "columns": [
                        { "key": "name", "label": "Check" },
                        { "key": "result", "label": "Result", "align": "end" }
                    ],
                    "rows": [
                        { "name": "Build", "result": "Pass" },
                        { "name": "Test", "result": "Pass" }
                    ],
                    "caption": "Latest run"
                }),
            ),
            (
                "task-progress",
                json!({
                    "title": "Release preparation",
                    "value": 0.6,
                    "progress_label": "Three of five steps",
                    "current_step": "Running the full suite.",
                    "state_label": "In progress",
                    "state": "running"
                }),
            ),
        ]);
        for (template_id, params) in cases {
            store.resolve(template_id, params).await.unwrap();
        }
        assert_eq!(store.list().await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn view_show_update_patch_clear_lifecycle_broadcasts() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(
            dir.path().join("progress.json"),
            serde_json::to_vec(&json!({
                "id": "progress",
                "title": "Progress",
                "params_schema": { "value": "number", "label": "string" },
                "spec": {
                    "type": "progress",
                    "value": "{{value}}",
                    "label": "{{label}}"
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let templates = TemplateStore::load(dir.path().to_path_buf()).await.unwrap();
        let (broadcaster, mut broadcasts) = broadcast::channel(8);
        let log = BroadcastLog::default();
        let views = ViewManager::new(templates, broadcaster, log.clone());

        let shown = views
            .show(
                Some("progress".to_string()),
                None,
                Some(json!({ "value": 0.2, "label": "Starting" })),
                Some("view-test".to_string()),
                "canvas".to_string(),
            )
            .await
            .unwrap();
        assert_eq!(shown.spec["value"], 0.2);
        assert!(matches!(
            broadcasts.recv().await.unwrap(),
            HostToClient::ViewUpsert { .. }
        ));

        let updated = views
            .update(
                "view-test",
                Some(json!({ "value": 0.8 })),
                Some(json!([
                    { "op": "replace", "path": "/label", "value": "Nearly done" }
                ])),
            )
            .await
            .unwrap();
        assert_eq!(updated.spec["value"], 0.8);
        assert_eq!(updated.spec["label"], "Nearly done");
        assert_eq!(views.snapshot().await, vec![updated]);
        assert!(matches!(
            broadcasts.recv().await.unwrap(),
            HostToClient::ViewUpsert { .. }
        ));

        views.clear("view-test").await.unwrap();
        assert!(views.snapshot().await.is_empty());
        assert!(matches!(
            broadcasts.recv().await.unwrap(),
            HostToClient::ViewRemoved { .. }
        ));
        assert!(log.recent().iter().any(|event| matches!(
            event,
            HostToClient::ViewRemoved { instance_id } if instance_id == "view-test"
        )));
    }
}
