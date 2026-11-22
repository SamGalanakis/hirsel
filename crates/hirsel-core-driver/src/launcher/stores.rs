//! The launcher's content-addressed objects on disk: the code store's recipes and the
//! assembly records with their images. Each is named by the hash of its own content, so
//! keeping them is caching, not state (launch.md "The launcher").
use super::{Image, ImageFile, Images};
use crate::code_store::CodeStore;
use crate::host::records::Records;
use hirsel_core_ids::ContentHash;
use std::collections::BTreeSet;
use std::io;
use std::path::Path;

pub(crate) struct Stores {
    records: Records,
    code: CodeStore,
    recipe_bytes: usize,
}

impl Stores {
    /// The objects under `state`: assembly records and images in `objects`, recipes in the
    /// code store `code`.
    pub(crate) fn open(state: &Path, recipe_bytes: usize) -> io::Result<Self> {
        let objects = state.join("objects");
        std::fs::create_dir_all(&objects)?;
        Ok(Self {
            records: Records::open(&objects)?,
            code: CodeStore::new(state.join("code")),
            recipe_bytes,
        })
    }
}

impl Images for Stores {
    fn resolve(&self, assembly: [u8; 32]) -> Result<Image, String> {
        let record = self
            .records
            .assembly(ContentHash(assembly))
            .map_err(|e| format!("assembly record: {e}"))?;
        let recipe = crate::host::read_recipe(&self.code, record.recipe, self.recipe_bytes)
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

    fn collect(&self, keep: &BTreeSet<[u8; 32]>) -> io::Result<u64> {
        let mut assemblies = BTreeSet::new();
        let mut images = BTreeSet::new();
        for &root in keep {
            // A root that names no record keeps nothing more.
            if let Ok(record) = self.records.assembly(ContentHash(root)) {
                assemblies.insert(record.identity);
                images.insert(record.digest);
            }
        }
        self.records.collect(&assemblies, &images)
    }
}
