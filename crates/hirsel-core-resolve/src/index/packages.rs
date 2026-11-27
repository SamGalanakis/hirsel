//! The package table: each package, its root module and its dependency rows.
use super::*;

impl NameIndex {
    pub fn add_package(
        &mut self,
        name: Symbol,
        short: Symbol,
        version: Symbol,
        toolchain: bool,
    ) -> PackageKey {
        if let Some(&p) = self.package_ids.get(&(name, version)) {
            return p;
        }
        let id = PackageKey(self.packages.len() as u32);
        let root = ModuleId(self.modules.len() as u32);
        self.modules.push(Module {
            package: id,
            parent: None,
            name,
            depth: 0,
            merge: Rows::default(),
        });
        self.packages.push(Package {
            name,
            short,
            version,
            root,
            dependencies: Rows::default(),
            toolchain,
        });
        self.package_ids.insert((name, version), id);
        id
    }
    pub fn package(&self, name: Symbol, version: Symbol) -> Option<PackageKey> {
        self.package_ids.get(&(name, version)).copied()
    }
    /// Whether the installed toolchain supplied `package` to this compilation.
    pub fn supplied_by_toolchain(&self, package: &hirsel_core_ids::PackageId) -> bool {
        let Some((name, version)) = self
            .symbols
            .find(&package.name)
            .zip(self.symbols.find(&package.version))
        else {
            return false;
        };
        self.package(name, version)
            .is_some_and(|key| self.packages[key.0 as usize].toolchain)
    }
    /// Sets `p`'s dependencies, each with the alias its source names it by.
    pub fn set_dependencies(
        &mut self,
        p: PackageKey,
        ds: impl IntoIterator<Item = (Symbol, PackageKey)>,
    ) {
        let start = self.dependencies.len() as u32;
        self.dependencies.extend(ds);
        self.packages[p.0 as usize].dependencies = Rows {
            start,
            end: self.dependencies.len() as u32,
        };
    }
}
