//! The launcher's content-addressed objects on disk: the code store's recipes and the
//! assembly records with their images. Each is named by the hash of its own content, so
//! keeping them is caching, not state (launch.md "The launcher").
use super::objects::Records;
use super::{Image, ImageFile, Images};
use crate::code_admission::ObjectKey;
use crate::code_store::{CodeStore, ObjectKind};
use hirsel_core_ids::ContentHash;
use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::sync::Arc;

pub(crate) struct Stores {
    records: Arc<Records>,
    code: CodeStore,
}

impl Stores {
    /// The objects under `state`: assembly records and images in `objects`, recipes in the
    /// code store `code`.
    pub(crate) fn open(state: &Path) -> io::Result<Self> {
        let objects = state.join("objects");
        std::fs::create_dir_all(&objects)?;
        Ok(Self {
            records: Arc::new(Records::open(&objects)?),
            code: CodeStore::new(state.join("code")),
        })
    }
}

impl Stores {
    /// The assembly records, which admission writes and launches read.
    pub(crate) fn records(&self) -> Arc<Records> {
        self.records.clone()
    }
}

impl Images for Stores {
    fn resolve(&self, assembly: [u8; 32]) -> Result<Image, String> {
        let record = self
            .records
            .assembly(ContentHash(assembly))
            .map_err(|e| format!("assembly record: {e}"))?;
        let recipe = super::read_recipe(&self.code, record.recipe, None)
            .map_err(|e| format!("recipe: {e}"))?;
        let verified = self
            .records
            .open_image(record.digest)
            .map_err(|e| format!("image: {e}"))?;
        Ok(Image {
            path: self.records.image_path(record.digest),
            executable: ImageFile::Verified(verified),
            digest: record.digest.0,
            n_place: recipe.launch.n_place,
            prefix: crate::exec_launch::arguments(&recipe).collect(),
        })
    }

    fn collect(
        &self,
        keep: &BTreeSet<[u8; 32]>,
        reserved: &dyn Fn(ObjectKey) -> bool,
    ) -> io::Result<u64> {
        let mut assemblies = BTreeSet::new();
        let mut images = BTreeSet::new();
        let mut code = BTreeSet::new();
        for &root in keep {
            // A root that names no record keeps nothing more.
            let Ok(record) = self.records.assembly(ContentHash(root)) else {
                continue;
            };
            assemblies.insert(record.identity);
            images.insert(record.digest);
            // A kept record whose recipe cannot be read stops the collection: what it
            // lists is unknown, so no code object may go.
            let recipe = super::read_recipe(&self.code, record.recipe, None)?;
            let key = key_of;
            code.insert(key(ObjectKind::Recipe, record.recipe));
            code.extend(recipe.groups.iter().map(|g| key(ObjectKind::Group, g.hash)));
            for pair in &recipe.bodies {
                code.insert(key(ObjectKind::Body, pair.body.hash));
                code.insert(key(ObjectKind::Annex, pair.annex.hash));
            }
            code.extend(recipe.values.iter().map(|v| key(ObjectKind::Value, v.hash)));
            code.extend(
                recipe
                    .foreign_archives
                    .iter()
                    .map(|a| key(ObjectKind::ForeignArchive, a.hash)),
            );
        }
        let removed = self.records.collect(&assemblies, &images)?;
        let code_removed = self.code.collect(
            |kind, hash| {
                let object = key_of(kind, hash);
                code.contains(&object) || reserved(object)
            },
            PUBLISHED_FLOOR,
        )?;
        Ok(removed + code_removed)
    }
}

/// How long a published code object outlives collection without a root: an operator may
/// publish and admit it later, as git keeps unreachable objects for `gc.pruneExpire`.
const PUBLISHED_FLOOR: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

fn key_of(kind: ObjectKind, hash: ContentHash) -> ObjectKey {
    ObjectKey { kind, hash }
}
