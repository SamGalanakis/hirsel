use hirsel_engine_store::*;
use hirsel_syntax::{ContentHash, PackageId};
use std::sync::Arc;

struct InstalledRemote {
    remote: Arc<dyn ResultStore>,
    installed: Arc<dyn ResultStore>,
}

pub fn bind(remote: Arc<dyn ResultStore>) -> Arc<dyn ResultStore> {
    assert!(remote.verifier().is_none());
    Arc::new(InstalledRemote {
        remote,
        installed: store_by_name("none@1", None).unwrap(),
    })
}

impl ResultStore for InstalledRemote {
    fn admit_product_memory(&self, bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
        self.remote.admit_product_memory(bytes)
    }

    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
        self.remote.retention_scope()
    }

    fn capability(&self) -> hirsel_engine_store::ProviderCapability {
        self.remote.capability()
    }

    fn installed_package(
        &self,
        package: &PackageId,
        compiler: ContentHash,
    ) -> Result<Arc<InstalledPackage>, String> {
        self.installed.installed_package(package, compiler)
    }
    fn read_scope(&self) -> Option<Box<dyn Send + Sync>> {
        self.installed.read_scope()
    }
    fn get(&self, key: &EntryKey) -> Option<Bytes> {
        self.remote.get(key).or_else(|| self.installed.get(key))
    }
    fn put(&self, key: &EntryKey, value: &[u8]) -> hirsel_engine_store::WriteOutcome {
        self.remote.put(key, value)
    }
    fn lookup_product(&self, address: Address) -> Option<Association> {
        self.remote
            .lookup_product(address)
            .or_else(|| self.installed.lookup_product(address))
    }
    fn get_content(&self, reference: Reference) -> Option<Bytes> {
        self.remote
            .get_content(reference)
            .or_else(|| self.installed.get_content(reference))
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        self.remote.put_content(domain, bytes)
    }
    fn associate_product(&self, address: Address, action: Action, reference: Reference) {
        self.remote.associate_product(address, action, reference);
    }
    fn associate_written_product(&self, address: Address, action: Action, reference: Reference) {
        self.remote
            .associate_written_product(address, action, reference);
    }
    fn name(&self) -> &'static str {
        "remote-installed-fixture"
    }
}
