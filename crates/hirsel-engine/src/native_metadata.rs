use crate::{NativeDefinitionProduct, NativeSourceRoots};
use hirsel_query_products::retained::MetadataWeight;
use hirsel_syntax::DefPath;
pub trait NativeMetadataExt {
    fn foreign_use(&mut self, value: &crate::ForeignUse);
    fn source(&mut self, value: &NativeDefinitionProduct);
    fn roots(&mut self, roots: &mut NativeSourceRoots);
}
impl NativeMetadataExt for MetadataWeight {
    fn foreign_use(&mut self, value: &crate::ForeignUse) {
        self.tree::<DefPath, ()>(value.externs.len());
        for path in &value.externs {
            self.path(path);
        }
        self.map(&value.unmodeled, |w, path, external| {
            w.path(path);
            w.path(external);
        });
    }
    fn source(&mut self, value: &NativeDefinitionProduct) {
        let NativeDefinitionProduct {
            definition,
            signature: _,
            signature_dependencies,
            source,
            summary: _,
            summary_identity: _,
            lower: _,
            audit,
            materialized,
            representation,
            constant,
        } = value;
        self.path(definition);
        self.flat_vec(signature_dependencies);
        if let Some(generated) = &source.generated {
            self.add(generated.text.capacity());
        }
        if let Some(audit) = audit {
            self.path(&audit.definition);
            self.flat_vec(&audit.cleanup);
            self.flat_vec(&audit.closures);
            self.flat_vec(&audit.invocations);
        }
        self.vec(materialized, |w, (_, value)| w.materialized(value));
        if let Some(value) = representation {
            self.representation(value);
        }
        if let Some(value) = constant {
            self.materialized(value);
        }
    }
    fn roots(&mut self, roots: &mut NativeSourceRoots) {
        let NativeSourceRoots {
            definitions,
            environment,
            registry_revision: _,
            operations,
            builtins,
            instance_bodies,
            sources: _,
            foreign_links,
            foreign_unreplayable: _,
            foreign_threads,
        } = roots;
        self.vec(definitions, Self::path);
        self.add(environment.compiler_version.capacity());
        self.map(operations, |w, key, value| {
            w.path(&key.definition);
            w.path(&value.key.definition);
            w.flat_vec(&value.parameters);
        });
        self.map(builtins, |w, key, _| w.path(key));
        self.instance_bodies(instance_bodies);
        self.vec(foreign_links, |w, value| match value {
            crate::LinkInput::Library(name) => w.add(name.capacity()),
            crate::LinkInput::File(path) => w.add(path.capacity()),
        });
        self.map(foreign_threads, |w, key, _| {
            w.add(key.name.capacity());
            w.add(key.version.capacity());
        });
    }
}
