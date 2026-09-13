use super::*;

#[derive(Clone)]
pub(super) struct HirselPluginFactory;

impl PluginFactory for HirselPluginFactory {
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
    for (constructor, event_type) in [
        (THREAD_REPORTED_SOURCE_TYPE, THREAD_REPORTED_EVENT_TYPE),
        (THREAD_COMPLETED_SOURCE_TYPE, THREAD_COMPLETED_EVENT_TYPE),
        (THREAD_MESSAGE_SOURCE_TYPE, THREAD_MESSAGE_EVENT_TYPE),
        (THREAD_TURN_SOURCE_TYPE, THREAD_TURN_EVENT_TYPE),
    ] {
        let (namespace, name) = constructor
            .split_once('.')
            .expect("Thread trigger constructor has a namespace");
        resources
            .add_trigger_source_constructor(
                [namespace, name],
                lash::rlm::TypeExpr::Object(vec![lash::rlm::TypeField {
                    name: "thread_id".into(),
                    ty: lash::rlm::TypeExpr::Int,
                    optional: false,
                }]),
                lash::rlm::NamedDataType::object(
                    event_type,
                    vec![
                        lash::rlm::TypeField {
                            name: "thread_id".into(),
                            ty: lash::rlm::TypeExpr::Int,
                            optional: false,
                        },
                        lash::rlm::TypeField {
                            name: "title".into(),
                            ty: lash::rlm::TypeExpr::Str,
                            optional: false,
                        },
                        lash::rlm::TypeField {
                            name: "payload".into(),
                            ty: lash::rlm::TypeExpr::Str,
                            optional: false,
                        },
                    ],
                )
                .expect("valid Thread event type"),
            )
            .expect("valid Thread trigger source");
    }
    lash::rlm::LashlangSurfaceContribution::new(
        lash::rlm::LashlangAbilities::default(),
        lash::rlm::LashlangLanguageFeatures::default(),
        resources,
    )
}
