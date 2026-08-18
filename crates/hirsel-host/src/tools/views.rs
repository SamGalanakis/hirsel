use super::ToolSuite;

impl ToolSuite {
    pub async fn views_show(
        &self,
        template_id: Option<String>,
        spec: Option<serde_json::Value>,
        params: Option<serde_json::Value>,
        instance_id: Option<String>,
        placement: String,
    ) -> anyhow::Result<hirsel_proto::ViewInstance> {
        self.views
            .show(template_id, spec, params, instance_id, placement)
            .await
    }

    pub async fn views_update(
        &self,
        instance_id: &str,
        params: Option<serde_json::Value>,
        patch: Option<serde_json::Value>,
    ) -> anyhow::Result<hirsel_proto::ViewInstance> {
        self.views.update(instance_id, params, patch).await
    }

    pub async fn views_clear(&self, instance_id: &str) -> anyhow::Result<()> {
        self.views.clear(instance_id).await
    }

    pub async fn views_list_templates(
        &self,
    ) -> anyhow::Result<Vec<crate::templates::TemplateSummary>> {
        self.views.templates().list().await
    }
}
