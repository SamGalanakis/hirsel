//! Runtime-selectable model state for the main agent and sub-agents.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub id: String,
    pub variant: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableModel {
    pub id: String,
    pub label: String,
    pub variants: Vec<String>,
    pub default_variant: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSnapshot {
    pub current: ModelSelection,
    pub available: Vec<AvailableModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentModel {
    pub id: String,
    pub label: String,
    pub variants: Vec<String>,
    pub enabled_variants: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentProviderModels {
    pub provider: String,
    pub label: String,
    pub models: Vec<SubagentModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentModelCatalog {
    pub providers: Vec<SubagentProviderModels>,
}
