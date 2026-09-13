use super::*;

fn roster(
    dir: &tempfile::TempDir,
    store: &ConfigStore,
    provider: ProviderMode,
) -> ProviderRosterState {
    ProviderRosterState::new(
        store.clone(),
        &crate::boot_provider::BootProvider::env_default(provider),
        Some(dir.path().to_path_buf()),
    )
}

async fn state(
    dir: &tempfile::TempDir,
    provider: ProviderMode,
    configured_model: &str,
) -> ModelSelectionState {
    let store = store(dir).await;
    let roster = roster(dir, &store, provider);
    ModelSelectionState::load(provider, store, roster, configured_model)
        .await
        .unwrap()
}

async fn store(dir: &tempfile::TempDir) -> ConfigStore {
    ConfigStore::load(
        dir.path().join("hirsel.toml"),
        std::path::Path::new("/docs/hirsel-config.md"),
        &crate::host_config::EnvBootstrap::default(),
    )
    .await
    .unwrap()
}

#[test]
fn registry_validates_models_and_variants() {
    let selected = validate_selection(ProviderMode::Codex, "gpt-5.6-sol", "high").unwrap();
    assert_eq!(selected.id, "gpt-5.6-sol");
    assert_eq!(selected.variant, "high");
    assert!(validate_selection(ProviderMode::Codex, "gpt-5.6-luna", "max").is_err());
    assert!(validate_selection(ProviderMode::Codex, "gpt-5", "high").is_err());
    assert!(validate_selection(ProviderMode::Codex, "gpt-5.6-sol", "impossible").is_err());
}

#[tokio::test]
async fn astra_is_selectable_without_changing_the_default_or_fork_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(&dir, ProviderMode::Codex, "gpt-5.6-sol").await;
    assert_eq!(state.current().id, "gpt-5.6-sol");
    let snapshot = state.snapshot();
    let astra = snapshot
        .model
        .available
        .iter()
        .find(|model| model.id == "gpt-6-astra")
        .unwrap();
    assert_eq!(
        astra.variants,
        ["low", "medium", "high", "xhigh", "max", "ultra"]
    );
    assert_eq!(astra.default_variant, "medium");
    for effort in &astra.variants {
        let selection = state.validate(&astra.id, effort).unwrap();
        let spec = model_spec(ProviderMode::Codex, &selection).unwrap();
        assert_eq!(spec.variant, ReasoningSelection::Effort(effort.clone()));
        assert_eq!(spec.limits.context_window_tokens.get(), 272_000);
        assert_eq!(spec.capability.reasoning.unwrap().efforts, astra.variants);
    }
    assert!(state.validate(&astra.id, "impossible").is_err());
    assert!(validate_fork(ProviderMode::Codex, &astra.id, "high").is_err());
    state
        .persist_and_select(state.validate(&astra.id, "ultra").unwrap())
        .await
        .unwrap();
    let store = store(&dir).await;
    let roster = roster(&dir, &store, ProviderMode::Codex);
    let reloaded = ModelSelectionState::load(ProviderMode::Codex, store, roster, "gpt-5.6-sol")
        .await
        .unwrap();
    assert_eq!(
        reloaded.current(),
        ModelSelection {
            id: "gpt-6-astra".into(),
            variant: "ultra".into()
        }
    );
}

#[test]
fn codex_fork_registry_defaults_to_luna_max_and_can_escalate_to_sol() {
    let models = available_fork_models(ProviderMode::Codex);
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["gpt-5.6-luna", "gpt-5.6-sol"]
    );
    assert_eq!(
        models[0].variants,
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(models[0].default_variant, "max");
    assert_eq!(
        default_fork_selection(ProviderMode::Codex),
        Some(ModelSelection {
            id: "gpt-5.6-luna".to_string(),
            variant: "max".to_string(),
        })
    );
    assert!(validate_fork_selection(ProviderMode::Codex, "gpt-5.6-sol", "high").is_ok());
}

#[test]
fn registries_are_scoped_to_their_provider() {
    assert_eq!(
        available_models(ProviderMode::OpenRouter)
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>(),
        vec!["google/gemini-3.7-flash".to_string()]
    );
    // A model from the other provider's registry is unknown here, and vice
    // versa; Anthropic mode offers nothing selectable at all.
    assert!(validate_selection(ProviderMode::OpenRouter, "gpt-5.6-sol", "high").is_err());
    assert!(validate_selection(ProviderMode::Codex, "google/gemini-3.7-flash", "default").is_err());
    assert!(available_models(ProviderMode::Anthropic).is_empty());
}

#[test]
fn openrouter_offers_a_single_provider_default_variant() {
    let models = available_models(ProviderMode::OpenRouter);
    let entry = models.first().expect("OpenRouter registry entry");
    assert_eq!(entry.label, "Gemini 3.7 Flash");
    assert_eq!(entry.variants, vec!["default".to_string()]);
    assert_eq!(entry.default_variant, "default");
    assert!(validate_selection(ProviderMode::OpenRouter, &entry.id, "high").is_err());
}

#[tokio::test]
async fn persistence_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let selection_state = state(&dir, ProviderMode::Codex, "gpt-5.6-sol").await;
    selection_state
        .persist_and_select(ModelSelection {
            id: "gpt-5.6-sol".to_string(),
            variant: "max".to_string(),
        })
        .await
        .unwrap();

    let reloaded = state(&dir, ProviderMode::Codex, "gpt-5.6-sol").await;
    assert_eq!(
        reloaded.current(),
        ModelSelection {
            id: "gpt-5.6-sol".to_string(),
            variant: "max".to_string(),
        }
    );
}

#[tokio::test]
async fn stale_or_invalid_config_falls_back_without_bricking_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir).await;
    store
        .set_model_selection(hirsel_proto::AgentSlot::Main, "retired-model", "impossible")
        .await
        .unwrap();
    let roster = roster(&dir, &store, ProviderMode::Codex);
    let state = ModelSelectionState::load(ProviderMode::Codex, store, roster, "gpt-5.6-sol")
        .await
        .unwrap();
    assert_eq!(
        state.current(),
        ModelSelection {
            id: "gpt-5.6-sol".to_string(),
            variant: "medium".to_string(),
        }
    );
}

#[tokio::test]
async fn a_selection_from_another_provider_falls_back_to_the_configured_model() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir).await;
    // What `data/hirsel.toml` holds after running in Codex mode; booting in
    // OpenRouter mode must degrade to the configured default, not fail.
    store
        .set_model_selection(hirsel_proto::AgentSlot::Main, "gpt-5.6-sol", "high")
        .await
        .unwrap();
    let roster = roster(&dir, &store, ProviderMode::OpenRouter);
    let state = ModelSelectionState::load(
        ProviderMode::OpenRouter,
        store,
        roster,
        "google/gemini-3.7-flash",
    )
    .await
    .unwrap();
    assert_eq!(
        state.current(),
        ModelSelection {
            id: "google/gemini-3.7-flash".to_string(),
            variant: "default".to_string(),
        }
    );
}

async fn router_state(dir: &tempfile::TempDir, booted: ProviderMode) -> ModelSelectionState {
    let store = store(dir).await;
    let roster = roster(dir, &store, booted);
    roster
        .add(
            "router",
            "Router",
            "https://example.invalid/v1",
            "sk-fake-key",
            "some/model",
        )
        .await
        .unwrap();
    let choice = roster.selection_for("router").unwrap();
    roster
        .point_agent_at(
            AgentSlot::Main,
            &choice,
            &ModelSelection {
                id: "some/model".to_string(),
                variant: "default".to_string(),
            },
        )
        .await
        .unwrap();
    ModelSelectionState::load(booted, store, roster, "gpt-5.6-sol")
        .await
        .unwrap()
}

#[tokio::test]
async fn an_openai_compatible_provider_takes_any_non_empty_model_id() {
    let dir = tempfile::tempdir().unwrap();
    let state = router_state(&dir, ProviderMode::Codex).await;

    let snapshot = state.snapshot();
    assert!(snapshot.model.free_text_model);
    assert!(snapshot.model.available.is_empty());
    assert_eq!(snapshot.model.provider_id.as_deref(), Some("router"));
    assert_eq!(snapshot.model.current.id, "some/model");
    assert_eq!(snapshot.model.current.variant, "default");

    // Any id the endpoint might offer is accepted, and the variant is the
    // provider's own: the host has no effort ladder to promise.
    let accepted = state.validate("vendor/brand-new-model", "high").unwrap();
    assert_eq!(accepted.id, "vendor/brand-new-model");
    assert_eq!(accepted.variant, "default");
    // Shape is the only thing left to check.
    for rejected in ["", "  ", " model", "model "] {
        assert!(
            state.validate(rejected, "default").is_err(),
            "accepted {rejected:?}"
        );
    }
}

#[tokio::test]
async fn the_codex_registry_stays_curated_when_it_is_the_selected_provider() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir).await;
    let roster = roster(&dir, &store, ProviderMode::OpenRouter);
    let choice = roster.selection_for("codex").unwrap();
    roster
        .point_agent_at(
            AgentSlot::Main,
            &choice,
            &ModelSelection {
                id: "gpt-5.6-sol".to_string(),
                variant: "medium".to_string(),
            },
        )
        .await
        .unwrap();
    let state = ModelSelectionState::load(
        ProviderMode::OpenRouter,
        store,
        roster,
        "google/gemini-3.7-flash",
    )
    .await
    .unwrap();

    let snapshot = state.snapshot();
    assert!(!snapshot.model.free_text_model);
    assert_eq!(snapshot.model.provider_id.as_deref(), Some("codex"));
    assert_eq!(
        snapshot
            .model
            .available
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["gpt-5.6-sol", "gpt-6-astra"]
    );
    assert!(state.validate("gpt-5.6-luna", "max").is_err());
    assert!(state.validate("gpt-5.6-sol", "impossible").is_err());
    assert!(state.validate("gpt-5.6-sol", "xhigh").is_ok());
}

#[tokio::test]
async fn a_main_provider_the_host_did_not_boot_on_still_reaches_the_live_spec() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = router_state(&dir, ProviderMode::Codex).await;
    // Stored, reported...
    assert_eq!(
        elsewhere.snapshot().model.provider_id.as_deref(),
        Some("router")
    );
    assert_eq!(elsewhere.current().id, "some/model");
    // ...and run: the session is rebound to the chosen provider before its next
    // turn, so the spec it runs is that provider's own.
    let spec = elsewhere.model_spec().unwrap();
    assert_eq!(spec.id, "some/model");
    assert!(elsewhere.applies_to_live_session());

    // An agent still on the booted provider runs exactly what it selected.
    let booted_dir = tempfile::tempdir().unwrap();
    let booted = state(&booted_dir, ProviderMode::Codex, "gpt-5.6-sol").await;
    booted
        .persist_and_select(ModelSelection {
            id: "gpt-5.6-sol".to_string(),
            variant: "xhigh".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(booted.model_spec().unwrap().variant.effort(), Some("xhigh"));
    assert!(booted.applies_to_live_session());

    // A provider with no stored key is no route at all: the selection is kept
    // and reported, while the live session stays on what it can still reach.
    let keyless_dir = tempfile::tempdir().unwrap();
    let store = store(&keyless_dir).await;
    let roster = roster(&keyless_dir, &store, ProviderMode::Codex);
    roster
        .add(
            "keyless",
            "Keyless",
            "https://example.invalid/v1",
            "",
            "k/m",
        )
        .await
        .unwrap();
    let choice = roster.selection_for("keyless").unwrap();
    roster
        .point_agent_at(
            AgentSlot::Main,
            &choice,
            &ModelSelection {
                id: "k/m".to_string(),
                variant: "default".to_string(),
            },
        )
        .await
        .unwrap();
    let keyless = ModelSelectionState::load(ProviderMode::Codex, store, roster, "gpt-5.6-sol")
        .await
        .unwrap();
    assert_eq!(keyless.current().id, "k/m");
    assert_eq!(keyless.model_spec().unwrap().id, "gpt-5.6-sol");
    assert!(!keyless.applies_to_live_session());
}

#[test]
fn model_spec_carries_the_selected_effort_and_capability() {
    let spec = model_spec(
        ProviderMode::Codex,
        &ModelSelection {
            id: "gpt-5.6-sol".to_string(),
            variant: "high".to_string(),
        },
    )
    .unwrap();
    assert_eq!(spec.id, "gpt-5.6-sol");
    assert_eq!(spec.variant.effort(), Some("high"));
    assert_eq!(spec.limits.context_window_tokens.get(), 200_000);
    assert!(
        spec.capability
            .reasoning
            .expect("reasoning capability")
            .efforts
            .contains(&"high".to_string())
    );
}

#[test]
fn codex_fork_model_spec_accepts_the_default_luna_lane() {
    let spec = model_spec(
        ProviderMode::Codex,
        &ModelSelection {
            id: "gpt-5.6-luna".to_string(),
            variant: "max".to_string(),
        },
    )
    .unwrap();

    assert_eq!(spec.id, "gpt-5.6-luna");
    assert_eq!(spec.variant.effort(), Some("max"));
}

#[test]
fn openrouter_model_spec_defers_reasoning_to_the_provider() {
    let spec = model_spec(
        ProviderMode::OpenRouter,
        &ModelSelection {
            id: "google/gemini-3.7-flash".to_string(),
            variant: "default".to_string(),
        },
    )
    .unwrap();
    assert_eq!(spec.id, "google/gemini-3.7-flash");
    assert_eq!(spec.variant, ReasoningSelection::ProviderDefault);
    assert_eq!(spec.limits.context_window_tokens.get(), 1_000_000);
    assert!(spec.capability.reasoning.is_none());
}
