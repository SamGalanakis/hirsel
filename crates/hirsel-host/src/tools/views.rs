use super::ToolSuite;

impl ToolSuite {
    #[allow(clippy::too_many_arguments)]
    pub async fn views_show(
        &self,
        expected_history: &str,
        thread_id: u64,
        template_id: Option<String>,
        spec: Option<serde_json::Value>,
        params: Option<serde_json::Value>,
        instance_id: Option<String>,
        placement: String,
    ) -> anyhow::Result<hirsel_proto::ViewInstance> {
        self.views
            .show(
                expected_history,
                thread_id,
                template_id,
                spec,
                params,
                instance_id,
                placement,
            )
            .await
    }

    pub async fn views_update(
        &self,
        expected_history: &str,
        thread_id: u64,
        instance_id: &str,
        params: Option<serde_json::Value>,
        patch: Option<serde_json::Value>,
    ) -> anyhow::Result<hirsel_proto::ViewInstance> {
        self.views
            .update(expected_history, thread_id, instance_id, params, patch)
            .await
    }

    pub async fn views_clear(
        &self,
        expected_history: &str,
        thread_id: u64,
        instance_id: &str,
    ) -> anyhow::Result<()> {
        self.views
            .clear(expected_history, thread_id, instance_id)
            .await
    }

    pub async fn views_list_templates(
        &self,
    ) -> anyhow::Result<Vec<crate::templates::TemplateSummary>> {
        self.views.templates().list().await
    }
}

impl ToolSuite {
    pub(crate) async fn view(&self, id: &str) -> Option<hirsel_proto::ViewInstance> {
        self.views.get(id).await
    }
}
