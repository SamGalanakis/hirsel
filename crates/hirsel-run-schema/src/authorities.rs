//! The `io.authorities` grant (determinism.md, Authority grants): the store authorities
//! `io.authority` returns, by name. Validation is hirsel-run-config's; the provider-specific
//! checks are hirsel-store-authority's.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One authority: its provider, its floor and the provider's settings. A member another
/// provider takes is refused by validation, never ignored.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityGrant {
    pub provider: String,
    /// The name of a level the provider declares.
    pub floor: String,
    /// `authority.log@1`: the virtual path of a granted writable Dir.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
    /// `authority.log@1`: a canonical relative directory within `mount`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `authority.sim@1`: the seed every simulated fault is drawn from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// `authority.sim@1`: `"log"` or `"objects"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub represent: Option<String>,
    /// `authority.sim@1`: the declared levels, weakest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub levels: Option<Vec<String>>,
    /// `authority.sim@1`: each injected fault's `fail_one_in` count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faults: Option<BTreeMap<String, u64>>,
}
