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

/// A catalog whose native worker runs on `providers`, with the row's own
/// enable switch left on.
fn catalog_with_worker_providers(providers: &[&str]) -> SubagentModelCatalog {
    let mut catalog = registry_catalog();
    catalog.native_worker.eligible_provider_ids =
        providers.iter().map(|id| (*id).to_string()).collect();
    catalog.native_worker.provider_id = providers
        .iter()
        .find(|id| **id == crate::providers::NATIVE_WORKER_DEFAULT_PROVIDER_ID)
        .map(|id| (*id).to_string());
    catalog.native_worker.unavailable_reason =
        providers.is_empty().then(|| "none configured".to_string());
    catalog
}

fn lash_delegation() -> Value {
    json!({
        "agent":"lash",
        "title":"Fix it",
        "brief":"Repair and verify the bug.",
        "artifact_ids":[]
    })
}

#[test]
fn native_lash_schema_exists_only_for_usable_providers() {
    let base = lash_delegation();
    let absent =
        SubagentModelState::delegation_input_schema_for(&catalog_with_worker_providers(&[]));
    assert_eq!(
        absent["properties"]["agent"]["enum"],
        json!(["host", "claude", "codex"])
    );
    assert!(
        !jsonschema::JSONSchema::compile(&absent)
            .unwrap()
            .is_valid(&base)
    );

    let schema =
        SubagentModelState::delegation_input_schema_for(&catalog_with_worker_providers(&[
            "openrouter",
            "local",
        ]));
    let validator = jsonschema::JSONSchema::compile(&schema).unwrap();
    assert!(
        validator.is_valid(&base),
        "OpenRouter has the curated default"
    );
    assert!(validator.is_valid(&json!({
        "agent":"lash", "provider_id":"openrouter", "model":"other/model",
        "variant":"default", "title":"Fix it", "brief":"Repair it", "artifact_ids":[]
    })));
    assert!(validator.is_valid(&json!({
        "agent":"lash", "provider_id":"local", "model":"local-model",
        "title":"Fix it", "brief":"Repair it", "artifact_ids":[]
    })));
    assert!(!validator.is_valid(&json!({
        "agent":"lash", "provider_id":"local",
        "title":"Fix it", "brief":"Repair it", "artifact_ids":[]
    })));
    assert!(!validator.is_valid(&json!({
        "agent":"lash", "provider_id":"missing", "model":"m",
        "title":"Fix it", "brief":"Repair it", "artifact_ids":[]
    })));
}

/// The Owner's switch, not the roster, decides whether the branch exists:
/// a configured provider is not consent to delegate to the worker.
#[test]
fn disabling_the_native_worker_drops_the_lash_branch() {
    let mut catalog = catalog_with_worker_providers(&["openrouter"]);
    catalog.native_worker.enabled = false;
    let schema = SubagentModelState::delegation_input_schema_for(&catalog);
    assert_eq!(
        schema["properties"]["agent"]["enum"],
        json!(["host", "claude", "codex"])
    );
    assert!(
        !jsonschema::JSONSchema::compile(&schema)
            .unwrap()
            .is_valid(&lash_delegation())
    );
    // The CLI lanes beside it are untouched by the native worker's switch.
    assert!(jsonschema::JSONSchema::compile(&schema).unwrap().is_valid(
        &json!({"agent":"codex","model":"gpt-5.6-sol","variant":"high","title":"Work","brief":"Do it","artifact_ids":[]})
    ));
}

/// A store with one keyed OpenAI-compatible instance, so the native worker
/// section has somewhere to run.
async fn worker_state(dir: &tempfile::TempDir, provider_id: &str) -> SubagentModelState {
    let state = test_state(dir).await;
    let path = dir.path().join("hirsel.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(&format!(
        "\n[providers.{provider_id}]\nkind = \"openai_compatible\"\nbase_url = \"https://example.invalid/v1\"\napi_key = \"sk-test\"\ndefault_model = \"vendor/model\"\n"
    ));
    std::fs::write(&path, text).unwrap();
    state
}

#[tokio::test]
async fn native_worker_section_reports_the_route_and_its_absence() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir).await;

    // Nothing configured: present, but honest that it cannot run.
    let worker = state.snapshot().native_worker;
    assert!(worker.enabled);
    assert_eq!(worker.provider_id, None);
    assert!(worker.eligible_provider_ids.is_empty());
    assert_eq!(worker.model, crate::providers::NATIVE_WORKER_DEFAULT_MODEL);
    assert!(worker.unavailable_reason.is_some());

    // The default instance: routed, and available.
    let state = worker_state(&dir, crate::providers::NATIVE_WORKER_DEFAULT_PROVIDER_ID).await;
    let worker = state.snapshot().native_worker;
    assert_eq!(
        worker.provider_id.as_deref(),
        Some(crate::providers::NATIVE_WORKER_DEFAULT_PROVIDER_ID)
    );
    assert_eq!(
        worker.eligible_provider_ids,
        [crate::providers::NATIVE_WORKER_DEFAULT_PROVIDER_ID]
    );
    assert_eq!(worker.unavailable_reason, None);

    // An instance that is not the default one still hosts the worker; it
    // just cannot be the implicit route, so this is not "unavailable".
    let dir = tempfile::tempdir().unwrap();
    let state = worker_state(&dir, "local").await;
    let worker = state.snapshot().native_worker;
    assert_eq!(worker.provider_id, None);
    assert_eq!(worker.eligible_provider_ids, ["local"]);
    assert_eq!(worker.unavailable_reason, None);
}

#[tokio::test]
async fn native_worker_toggle_and_override_round_trip_through_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let state = worker_state(&dir, crate::providers::NATIVE_WORKER_DEFAULT_PROVIDER_ID).await;

    let catalog = state
        .set_native_worker(true, Some("  vendor/other  "))
        .await
        .unwrap();
    assert_eq!(
        catalog.native_worker.model_override.as_deref(),
        Some("vendor/other")
    );
    assert_eq!(catalog.native_worker.model, "vendor/other");
    assert_eq!(
        catalog.native_worker.default_model,
        crate::providers::NATIVE_WORKER_DEFAULT_MODEL
    );

    // The override survives a reload of the same file.
    let reloaded = test_state(&dir).await.snapshot().native_worker;
    assert_eq!(reloaded.model, "vendor/other");
    assert!(reloaded.enabled);

    // Blank clears the override rather than storing an empty model.
    let catalog = state.set_native_worker(false, Some("   ")).await.unwrap();
    assert_eq!(catalog.native_worker.model_override, None);
    assert_eq!(
        catalog.native_worker.model,
        crate::providers::NATIVE_WORKER_DEFAULT_MODEL
    );
    assert!(!catalog.native_worker.enabled);
    let persisted = std::fs::read_to_string(dir.path().join("hirsel.toml")).unwrap();
    assert!(persisted.contains("[native_worker]"), "{persisted}");
    assert!(!persisted.contains("model = \"\""), "{persisted}");

    let reloaded = test_state(&dir).await.snapshot().native_worker;
    assert!(!reloaded.enabled);
    assert_eq!(reloaded.model_override, None);
}

/// The override goes through the same validation as an explicit delegate
/// `model`, so the two cannot disagree about what this route accepts.
#[tokio::test]
async fn native_worker_override_is_validated_like_a_delegate_model() {
    let dir = tempfile::tempdir().unwrap();
    let state = worker_state(&dir, crate::providers::NATIVE_WORKER_DEFAULT_PROVIDER_ID).await;
    for model in ["vendor/model", "vendor/model:free", "local-model"] {
        assert!(crate::model_selection::validate_free_text(model).is_ok());
        assert_eq!(
            state
                .set_native_worker(true, Some(model))
                .await
                .unwrap()
                .native_worker
                .model,
            model
        );
    }
    // Whitespace-only is not a model id; it clears the override instead of
    // being stored, and the shipped default stands.
    assert_eq!(
        state
            .set_native_worker(true, Some("   \n  "))
            .await
            .unwrap()
            .native_worker
            .model,
        crate::providers::NATIVE_WORKER_DEFAULT_MODEL
    );
}

/// A malformed `[native_worker]` section is a config typo, not a reason to
/// silently take the delegation target away.
#[tokio::test]
async fn invalid_native_worker_config_falls_back_to_the_shipped_row() {
    let dir = tempfile::tempdir().unwrap();
    let state = worker_state(&dir, crate::providers::NATIVE_WORKER_DEFAULT_PROVIDER_ID).await;
    let path = dir.path().join("hirsel.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[native_worker]\nenabled = \"yes\"\nmodel = 7\n");
    std::fs::write(&path, text).unwrap();
    let worker = state.snapshot().native_worker;
    assert!(worker.enabled);
    assert_eq!(worker.model_override, None);
    assert_eq!(worker.model, crate::providers::NATIVE_WORKER_DEFAULT_MODEL);
}
