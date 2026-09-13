//! Sub-agent model catalog tests: the curated CLI registry, the native
//! worker row, and the delegation contract all three feed.

use super::*;

async fn test_state(dir: &tempfile::TempDir) -> SubagentModelState {
    let store = ConfigStore::load(
        dir.path().join("hirsel.toml"),
        std::path::Path::new("/docs/hirsel-config.md"),
        &crate::host_config::EnvBootstrap::default(),
    )
    .await
    .unwrap();
    SubagentModelState::load(store)
}

#[test]
fn registry_defaults_and_variants_are_valid() {
    for provider in REGISTRY {
        assert!(!provider.models.is_empty());
        for model in provider.models {
            assert!(!model.variants.is_empty());
            assert!(model.variants.contains(&model.default_variant));
        }
    }
}

/// New choices do not reorder the existing CLI defaults or retune lanes.
#[test]
fn registry_preserves_existing_lanes_and_adds_supported_models() {
    let catalog = registry_catalog();
    let lanes = catalog
        .providers
        .iter()
        .flat_map(|provider| provider.models.iter())
        .map(|model| {
            (
                model.id.as_str(),
                model.enabled,
                model.enabled_variants.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lanes,
        [
            ("gpt-5.6-sol", true, vec!["high".to_string()]),
            ("gpt-5.6-luna", true, vec!["max".to_string()]),
            (
                "gpt-6-astra",
                true,
                vec!["low", "medium", "high", "xhigh", "max", "ultra"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            ),
            ("claude-opus-5", true, vec!["high".to_string()]),
            (
                "claude-fable-5-1",
                true,
                vec!["low", "medium", "high", "xhigh", "max"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            ),
        ]
    );
}

/// An explicit hirsel.toml override still wins over the shipped defaults.
#[tokio::test]
async fn overrides_win_over_default_enablement() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir).await;
    let is_enabled = |catalog: &SubagentModelCatalog, id: &str| {
        catalog
            .providers
            .iter()
            .flat_map(|provider| provider.models.iter())
            .find(|model| model.id == id)
            .unwrap()
            .enabled
    };

    let catalog = state.snapshot();
    assert!(is_enabled(&catalog, "gpt-5.6-luna"));

    let catalog = state
        .set("codex", "gpt-5.6-luna", false, &["max".to_string()])
        .await
        .unwrap();
    assert!(!is_enabled(&catalog, "gpt-5.6-luna"));

    let catalog = state
        .set("codex", "gpt-5.6-luna", true, &["max".to_string()])
        .await
        .unwrap();
    assert!(is_enabled(&catalog, "gpt-5.6-luna"));
}

#[tokio::test]
async fn persistence_round_trips_via_config_store() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir).await;
    state
        .set("claude", "claude-opus-5", false, &["high".to_string()])
        .await
        .unwrap();

    let reloaded = test_state(&dir).await;
    let catalog = reloaded.snapshot();
    let opus = catalog.providers[1]
        .models
        .iter()
        .find(|model| model.id == "claude-opus-5")
        .unwrap();
    assert!(!opus.enabled);
    assert_eq!(opus.enabled_variants, ["high"]);
}

#[tokio::test]
async fn stale_persisted_model_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir).await;
    let mut text = std::fs::read_to_string(dir.path().join("hirsel.toml")).unwrap();
    // Both a never-known model and a retired lane are ignored, not fatal.
    text.push_str(
        "\n[subagent_models.codex.retired-model]\nenabled = false\nenabled_variants = [\"high\"]\n\
         \n[subagent_models.codex.\"gpt-5.6-terra\"]\nenabled = true\nenabled_variants = [\"medium\"]\n\
         \n[subagent_models.claude.claude-sonnet-5]\nenabled = true\nenabled_variants = [\"medium\"]\n",
    );
    std::fs::write(dir.path().join("hirsel.toml"), text).unwrap();
    let catalog = state.snapshot();
    assert_eq!(
        catalog.providers[0]
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["gpt-5.6-sol", "gpt-5.6-luna", "gpt-6-astra"]
    );
    assert_eq!(
        catalog.providers[1]
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["claude-opus-5", "claude-fable-5-1"]
    );
}

#[tokio::test]
async fn resolve_defaults_and_rejects_disabled_unknown_and_bad_variants() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir).await;
    assert_eq!(
        state.resolve(AgentKind::Claude, None, None).unwrap(),
        ResolvedSubagentModel {
            model_id: "claude-opus-5".to_string(),
            variant: "high".to_string(),
        }
    );
    assert_eq!(
        state.resolve(AgentKind::Codex, None, None).unwrap(),
        ResolvedSubagentModel {
            model_id: "gpt-5.6-sol".to_string(),
            variant: "high".to_string(),
        }
    );
    assert_eq!(
        state
            .resolve(AgentKind::Codex, Some("gpt-5.6-luna"), None)
            .unwrap()
            .variant,
        "max"
    );
    state
        .set("claude", "claude-opus-5", false, &["high".to_string()])
        .await
        .unwrap();
    assert!(
        state
            .resolve(AgentKind::Claude, Some("claude-opus-5"), None)
            .unwrap_err()
            .to_string()
            .contains("enabled models: claude-fable-5-1")
    );
    assert!(
        state
            .resolve(AgentKind::Codex, Some("unknown"), None)
            .is_err()
    );
    // Efforts outside the lane are rejected: there is no per-task tuning.
    assert!(
        state
            .resolve(AgentKind::Codex, None, Some("xhigh"))
            .is_err()
    );
    assert!(state.resolve(AgentKind::Codex, None, Some("high")).is_ok());
}

#[tokio::test]
async fn delegation_schema_tracks_enabled_models_and_variants() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir).await;
    let schema = state.delegation_input_schema();
    let validator = jsonschema::JSONSchema::compile(&schema).unwrap();
    let spawn = |agent: &str, model: &str, effort: &str| {
        json!({
            "agent": agent,
            "model": model,
            "variant": effort,
            "title":"Research", "brief": "Research Linear triage.", "artifact_ids":[]
        })
    };

    assert!(
        validator
            .validate(&spawn("claude", "claude-opus-5", "high"))
            .is_ok()
    );
    assert!(
        validator
            .validate(&spawn("codex", "gpt-5.6-luna", "max"))
            .is_ok()
    );
    assert!(
        validator
            .validate(&spawn("codex", "gpt-5.6-luna", "high"))
            .is_err()
    );
    assert!(
        validator
            .validate(&spawn("claude", "claude-sonnet-5", "high"))
            .is_err()
    );

    state
        .set("claude", "claude-opus-5", false, &["high".to_string()])
        .await
        .unwrap();
    let validator = jsonschema::JSONSchema::compile(&state.delegation_input_schema()).unwrap();
    assert!(
        validator
            .validate(&spawn("claude", "claude-opus-5", "high"))
            .is_err()
    );
}

#[tokio::test]
async fn new_models_resolve_persist_and_refresh_the_delegation_schema() {
    for (agent, provider, id, default, efforts) in [
        (
            AgentKind::Codex,
            "codex",
            "gpt-6-astra",
            "medium",
            &["low", "medium", "high", "xhigh", "max", "ultra"][..],
        ),
        (
            AgentKind::Claude,
            "claude",
            "claude-fable-5-1",
            "high",
            &["low", "medium", "high", "xhigh", "max"][..],
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        assert_eq!(
            state.resolve(agent, Some(id), None).unwrap().variant,
            default
        );
        let input = |effort: &str| json!({"agent":provider,"model":id,"variant":effort,"title":"Work","brief":"Do the work","artifact_ids":[]});
        let validator = jsonschema::JSONSchema::compile(&state.delegation_input_schema()).unwrap();
        for effort in efforts {
            assert_eq!(
                state
                    .resolve(agent, Some(id), Some(effort))
                    .unwrap()
                    .variant,
                *effort
            );
            assert!(validator.is_valid(&input(effort)));
        }
        assert!(state.resolve(agent, Some(id), Some("impossible")).is_err());
        assert!(!validator.is_valid(&input("impossible")));
        assert!(
            state
                .set(provider, id, true, &["impossible".into()])
                .await
                .is_err()
        );

        state
            .set(provider, id, true, &["high".into()])
            .await
            .unwrap();
        let restricted = test_state(&dir).await;
        assert_eq!(
            restricted.resolve(agent, Some(id), None).unwrap().variant,
            "high"
        );
        assert!(restricted.resolve(agent, Some(id), Some("low")).is_err());
        let validator =
            jsonschema::JSONSchema::compile(&restricted.delegation_input_schema()).unwrap();
        assert!(validator.is_valid(&input("high")));
        assert!(!validator.is_valid(&input("low")));

        restricted
            .set(provider, id, false, &["high".into()])
            .await
            .unwrap();
        let disabled = test_state(&dir).await;
        assert!(disabled.resolve(agent, Some(id), None).is_err());
        assert!(
            !jsonschema::JSONSchema::compile(&disabled.delegation_input_schema())
                .unwrap()
                .is_valid(&input("high"))
        );
    }
}

#[tokio::test]
async fn set_rejects_empty_and_unknown_variant_sets() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir).await;
    assert!(
        state
            .set("codex", "gpt-5.6-sol", true, &[])
            .await
            .unwrap_err()
            .to_string()
            .contains("at least one")
    );
    assert!(
        state
            .set("codex", "gpt-5.6-sol", true, &["impossible".to_string()],)
            .await
            .unwrap_err()
            .to_string()
            .contains("unknown variants")
    );
}

/// The Native branch is always available: a Thread that names no agent, or
/// names `native`, runs on Hirsel's own session.
#[test]
fn native_branch_accepts_inherited_and_explicit_selectors() {
    let schema = SubagentModelState::delegation_input_schema_for(&registry_catalog());
    let validator = jsonschema::JSONSchema::compile(&schema).unwrap();
    let base = json!({"title":"Work","brief":"Do it","artifact_ids":[]});
    let with = |extra: Value| {
        let mut value = base.clone();
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        value
    };

    // Naming nothing is Native, inheriting this Thread's provider and model.
    assert!(validator.validate(&base).is_ok());
    assert!(validator.validate(&with(json!({"agent":"native"}))).is_ok());
    assert!(
        validator
            .validate(&with(
                json!({"agent":"native","provider_id":"local","model":"m","cwd":"/tmp"})
            ))
            .is_ok()
    );
    // Native has no reasoning variant, and provider_id is Native's alone.
    assert!(
        validator
            .validate(&with(json!({"agent":"native","variant":"high"})))
            .is_err()
    );
    assert!(
        validator
            .validate(&with(json!({"agent":"codex","provider_id":"local"})))
            .is_err()
    );
    assert!(
        validator
            .validate(&with(
                json!({"agent":"lash","provider_id":"local","model":"m"})
            ))
            .is_err()
    );
}
