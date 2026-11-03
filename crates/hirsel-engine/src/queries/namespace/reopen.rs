use super::*;

const SCOPED: Domain = Domain {
    kind: MANIFEST.kind,
    schema: 2,
};
pub(in crate::queries) struct ManifestDemand {
    pub declaration: DefPath,
    pub arguments: Vec<hirsel_types::GenericArgument>,
}

fn declaration(manifest: &Manifest, id: DefId) -> DefId {
    manifest
        .instances
        .instances
        .iter()
        .find_map(|(declaration, rows)| {
            rows.values()
                .any(|row| row.definition == id)
                .then_some(*declaration)
        })
        .unwrap_or(id)
}
impl NamespaceCapture {
    fn stored_manifest(&self, reference: Reference, limit: u64) -> Option<Manifest> {
        if ![MANIFEST, SCOPED].contains(&reference.domain) || reference.len > limit {
            return None;
        }
        let bytes = self.store.get_content(reference)?;
        if reference.domain.reference(&bytes) != Some(reference) {
            return None;
        }
        let manifest: Manifest = self.records.decode(&**self.anchors(), &bytes)?;
        if manifest.budget != self.limits.budget
            || manifest.depth != self.limits.call_depth
            || manifest.charging_version != hirsel_tir::CHARGING_VERSION
            || manifest.dependencies.len() != manifest.trace.len()
            || manifest.dependencies.iter().any(|(_, reference)| {
                reference.domain != GENERATION
                    || reference.len > limit
                    || !self
                        .store
                        .get_content(*reference)
                        .is_some_and(|bytes| GENERATION.reference(&bytes) == Some(*reference))
            })
        {
            return None;
        }
        Some(manifest)
    }
    fn scope_base(&self, manifest: &Manifest) -> Option<ContentHash> {
        let mut inputs = BTreeMap::new();
        for (id, _) in &manifest.dependencies {
            let id = declaration(manifest, *id);
            inputs.insert(
                id,
                (
                    self.bodies.get(&id)?.implementation,
                    self.snapshot.declaration_candidates(id)?,
                ),
            );
        }
        let bytes = self
            .records
            .encode(&*self.anchors, &(self.environment, inputs))?;
        Some(ContentHash::of_parts(
            "hirsel.generation.scope-inputs.v1",
            [bytes.as_slice()],
        ))
    }
    pub(in crate::queries) fn manifest_for_reads(
        &self,
        root: DefId,
        reads: &Reads,
        limit: u64,
    ) -> Option<Reference> {
        let mut manifest = self.stored_manifest(self.manifest, limit)?;
        let mut needed = BTreeSet::new();
        let mut pending = std::iter::once(root)
            .chain(reads.iter().filter_map(|(key, _)| key.definition()))
            .collect::<Vec<_>>();
        while let Some(id) = pending.pop() {
            if let Some(registration) = self.registrations.get(&id)
                && needed.insert(id)
            {
                pending.extend(
                    registration
                        .reads()
                        .iter()
                        .filter_map(|(key, _)| key.definition()),
                );
            }
        }
        manifest.dependencies.retain(|(id, _)| needed.contains(id));
        manifest.trace.retain(|id| needed.contains(id));
        manifest.instances.instances.retain(|_, rows| {
            rows.retain(|_, row| needed.contains(&row.definition));
            !rows.is_empty()
        });
        manifest.instances.declarations = manifest.instances.instances.keys().copied().collect();
        manifest.base = self.scope_base(&manifest)?;
        let bytes = self.records.encode(&*self.anchors, &manifest)?;
        if bytes.len() as u64 > limit {
            return None;
        }
        self.store
            .put_content(SCOPED, &bytes)
            .filter(|reference| SCOPED.reference(&bytes) == Some(*reference))
    }
    pub(in crate::queries) fn manifest_references(
        &self,
        reference: Reference,
        limit: u64,
    ) -> Option<Vec<Reference>> {
        let manifest = self.stored_manifest(reference, limit)?;
        Some(
            std::iter::once(reference)
                .chain(
                    manifest
                        .dependencies
                        .into_iter()
                        .map(|(_, reference)| reference),
                )
                .collect(),
        )
    }
    pub(in crate::queries) fn manifest_inputs_hold(
        &self,
        reference: Reference,
        limit: u64,
    ) -> bool {
        if reference.domain != SCOPED {
            return false;
        }
        let Some(manifest) = self.stored_manifest(reference, limit) else {
            return false;
        };
        if self.scope_base(&manifest) != Some(manifest.base) {
            return false;
        }
        let Some(current) = self.stored_manifest(self.manifest, limit) else {
            return false;
        };
        manifest
            .dependencies
            .iter()
            .all(|dependency| current.dependencies.contains(dependency))
            && manifest
                .instances
                .instances
                .iter()
                .all(|(declaration, rows)| {
                    self.instances
                        .instances
                        .get(declaration)
                        .is_some_and(|current| {
                            rows.iter().all(|(key, row)| current.get(key) == Some(row))
                        })
                })
    }
    pub(in crate::queries) fn manifest_demands(
        &self,
        reference: Reference,
        limit: u64,
    ) -> Option<Vec<ManifestDemand>> {
        if reference.domain != SCOPED {
            return None;
        }
        let manifest = self.stored_manifest(reference, limit)?;
        if self.scope_base(&manifest) != Some(manifest.base) {
            return None;
        }
        let mut dependencies = manifest
            .dependencies
            .iter()
            .copied()
            .collect::<BTreeMap<_, _>>();
        let mut demands = Vec::new();
        for id in &manifest.trace {
            dependencies.remove(id)?;
            let declaration = declaration(&manifest, *id);
            let arguments = manifest
                .instances
                .instances
                .get(&declaration)
                .and_then(|rows| rows.values().find(|row| row.definition == *id))
                .map_or_else(Vec::new, |row| row.arguments.clone());
            demands.push(ManifestDemand {
                declaration: declaration.path().clone(),
                arguments,
            });
        }
        dependencies.is_empty().then_some(demands)
    }
    /// Source observations are checked before any historical demand is replayed.
    pub(in crate::queries) fn manifest_source_reads_hold(
        &self,
        reference: Reference,
        limit: u64,
        reads: &Reads,
        mut observe: impl FnMut(&ReadKey) -> Option<ContentHash>,
    ) -> bool {
        if reference.domain != SCOPED {
            return false;
        }
        let Some(manifest) = self.stored_manifest(reference, limit) else {
            return false;
        };
        if self.scope_base(&manifest) != Some(manifest.base) {
            return false;
        }
        reads.iter().all(|(key, expected)| {
            if matches!(key, ReadKey::ValidationUniverse(_)) {
                return true;
            }
            if key.definition().is_some_and(|id| {
                manifest
                    .dependencies
                    .iter()
                    .any(|(generated, _)| *generated == id)
                    || id.path().0.name.starts_with("$comptime:")
                    || manifest.dependencies.iter().any(|(generated, _)| {
                        generation::task::GeneratorInput::private_path(generated.path())
                            == *id.path()
                    })
            }) {
                return true;
            }
            observe(key) == Some(*expected)
        })
    }
}
