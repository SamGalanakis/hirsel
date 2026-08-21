use super::*;

#[derive(Clone)]
pub(super) struct HirselProcessPluginFactory {
    pub(super) tools: ToolSuite,
    pub(super) notify: Arc<Notify>,
    /// Handed to the monitor engine so a monitor wake is triaged by a fork
    /// instead of turning the main Agent (ADR-0015).
    pub(super) fork_wake: crate::fork_wake::ForkWakeHandle,
}

impl PluginFactory for HirselProcessPluginFactory {
    fn id(&self) -> &'static str {
        "hirsel_processes"
    }

    fn extension_contributions(&self) -> Vec<PluginExtensionContribution> {
        match PluginExtensionContribution::new(
            lash::rlm::LASHLANG_SURFACE_EXTENSION_ID,
            hirsel_lashlang_surface(),
        ) {
            Ok(contribution) => vec![contribution],
            Err(error) => {
                tracing::warn!(%error, "failed to encode Hirsel lashlang surface contribution");
                Vec::new()
            }
        }
    }

    fn process_engine_contributions(
        &self,
        _ctx: &ProcessEngineContributionContext<'_>,
    ) -> Result<Vec<Arc<dyn ProcessEngine>>, PluginError> {
        Ok(vec![
            Arc::new(HirselSubagentEngine {
                tools: self.tools.clone(),
            }),
            Arc::new(HirselMonitorEngine {
                tools: self.tools.clone(),
                notify: Arc::clone(&self.notify),
                fork_wake: self.fork_wake.clone(),
            }),
        ])
    }

    fn build(&self, _ctx: &PluginSessionContext) -> Result<Arc<dyn SessionPlugin>, PluginError> {
        Ok(Arc::new(EmptyHirselSessionPlugin))
    }
}

pub(super) struct EmptyHirselSessionPlugin;

impl SessionPlugin for EmptyHirselSessionPlugin {
    fn id(&self) -> &'static str {
        "hirsel_processes"
    }

    fn register(&self, _reg: &mut PluginRegistrar) -> Result<(), PluginError> {
        Ok(())
    }
}

pub(super) fn hirsel_lashlang_surface() -> lash::rlm::LashlangSurfaceContribution {
    let mut resources = lash::rlm::LashlangHostCatalog::new();
    resources
        .add_trigger_source_constructor(
            ["timer", "Schedule"],
            lash::rlm::TypeExpr::Object(vec![
                lash::rlm::TypeField {
                    name: "label".into(),
                    ty: lash::rlm::TypeExpr::Str,
                    optional: false,
                },
                lash::rlm::TypeField {
                    name: "at".into(),
                    ty: lash::rlm::TypeExpr::Str,
                    optional: true,
                },
                lash::rlm::TypeField {
                    name: "in_secs".into(),
                    ty: lash::rlm::TypeExpr::Int,
                    optional: true,
                },
                lash::rlm::TypeField {
                    name: "every_secs".into(),
                    ty: lash::rlm::TypeExpr::Int,
                    optional: true,
                },
            ]),
            lash::rlm::NamedDataType::object(
                TIMER_EVENT_TYPE,
                vec![
                    lash::rlm::TypeField {
                        name: "label".into(),
                        ty: lash::rlm::TypeExpr::Str,
                        optional: false,
                    },
                    lash::rlm::TypeField {
                        name: "fired_at".into(),
                        ty: lash::rlm::TypeExpr::Str,
                        optional: false,
                    },
                    lash::rlm::TypeField {
                        name: "scheduled_at".into(),
                        ty: lash::rlm::TypeExpr::Str,
                        optional: false,
                    },
                    lash::rlm::TypeField {
                        name: "source_key".into(),
                        ty: lash::rlm::TypeExpr::Str,
                        optional: false,
                    },
                    lash::rlm::TypeField {
                        name: "subscription_key".into(),
                        ty: lash::rlm::TypeExpr::Str,
                        optional: false,
                    },
                ],
            )
            .expect("valid timer.Tick type"),
        )
        .expect("valid timer.Schedule trigger source");
    lash::rlm::LashlangSurfaceContribution::new(
        lash::rlm::LashlangAbilities::default(),
        lash::rlm::LashlangLanguageFeatures::default(),
        resources,
    )
}
