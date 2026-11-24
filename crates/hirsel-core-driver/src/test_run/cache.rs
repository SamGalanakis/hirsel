use crate::native::EntryDecl;
use crate::{NativeOptions, PackageProduct, Reachable, Store};
use hirsel_core_ids::{ContentHash, DefPath};
use std::{
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{ExitStatus, Output},
};

const TEST_PASS_SCHEMA: u32 = 3;

/// Reused passes of determined tests (PROG-4).
pub struct TestCache {
    store: Store,
}

impl TestCache {
    /// A cache that cannot be opened leaves every test to execute.
    pub fn open(root: &Path) -> Option<Self> {
        Some(Self {
            store: Store::new(root).ok()?,
        })
    }

    pub fn default_root() -> PathBuf {
        PackageProduct::default_cache().join("test-results")
    }

    pub(super) fn get(&self, key: ContentHash, observe: bool) -> Option<(Output, LawReport)> {
        let bytes = self.store.get(key).ok()??;
        let (TEST_PASS_SCHEMA, stdout, stderr, laws) = crate::decode(&bytes)? else {
            return None;
        };
        if observe && !matches!(laws, LawReport::Observed(_)) {
            return None;
        }
        Some((
            Output {
                status: ExitStatus::from_raw(0),
                stdout,
                stderr,
            },
            laws,
        ))
    }

    pub(super) fn put(&self, key: ContentHash, output: &Output, laws: &LawReport) {
        if let Ok(bytes) = crate::encode(&(TEST_PASS_SCHEMA, &output.stdout, &output.stderr, laws))
        {
            let _ = self.store.put_keyed(key, &bytes);
        }
    }
}

pub(super) fn test_key(
    test: &DefPath,
    decl: &EntryDecl,
    reached: &Reachable<'_>,
    site_digest: ContentHash,
    options: &NativeOptions,
) -> Option<ContentHash> {
    let address = reached.address(test).ok()?;
    let inputs = reached.native.as_ref()?;
    let environment = match &inputs.records {
        Some(records) => records.digest,
        None => {
            let env = crate::native::environment(options, inputs.toolchain.runtime).ok()?;
            ContentHash::of_parts("hirsel.core-environment.v1", [&env[..]])
        }
    };
    // A test may compare site words or write them in its output. Its annexes
    // are keyed separately from the address of its code.
    crate::native::key(
        "hirsel.test-pass.v1",
        &(
            TEST_PASS_SCHEMA,
            test,
            decl,
            address,
            site_digest,
            environment,
            inputs.toolchain.image,
            inputs.recipe.identity,
            inputs.toolchain.environment,
            crate::native_entry::policy(options),
            None::<u64>, // Seed, once test execution can select one.
        ),
    )
    .ok()
}
use super::LawReport;
