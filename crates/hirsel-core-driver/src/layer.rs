//! An image's library objects in two layers. An installed image ships the
//! objects its installation prebuilt in its own `native` store, which a build
//! only reads: the prefix may be read-only, or shared by every user of the
//! machine. An instance it does not ship is compiled into a side of the
//! build's content store named by the image's identity, and read from there
//! by every later build over that image. A read tries the shipped layer, then
//! the user's; every write goes to the user's. Object keys name no path, so
//! one key finds the same object in either layer.
use super::Store;
use hirsel_core_ids::ContentHash;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

impl Store {
    /// The library objects of the image `identity` whose store is `image`,
    /// for a build whose content store is `cas`: the image's shipped objects
    /// under the user's side for it, which alone is written.
    pub(crate) fn library(cas: &Path, image: &Path, identity: ContentHash) -> io::Result<Self> {
        let side = ContentHash::of_parts("hirsel.library-side.v1", [&identity.0[..]]);
        Ok(Self::cas(cas)?.side(side)?.over(image))
    }
    /// This store read under `image`'s shipped objects, which are never
    /// written, swept or touched: a missing directory is an empty layer.
    pub(crate) fn over(mut self, image: &Path) -> Self {
        self.shipped = Some(
            image
                .join(crate::native::recipe_build::LIBRARY_OBJECTS)
                .into(),
        );
        self
    }
    /// The file of `own`, a path of this store's own layer, in its shipped
    /// layer: a function of the name alone.
    pub(super) fn shipped_path(&self, own: &Path) -> Option<PathBuf> {
        let shipped = self.shipped.as_deref()?;
        Some(shipped.join(own.strip_prefix(&self.root).ok()?))
    }
    /// The file a link reads for the blob `address`: the own layer's when it
    /// is there, since a rebuild of a damaged shipped blob lands there, else
    /// the shipped layer's.
    pub(crate) fn object_path(&self, address: ContentHash) -> PathBuf {
        let own = self.blob_path(address);
        match self.shipped_path(&own) {
            Some(shipped) if fs::symlink_metadata(&own).is_err() && shipped.exists() => shipped,
            _ => own,
        }
    }
    /// The layers a read tries, in order, and whether each is this store's
    /// own (touched on a hit).
    pub(super) fn layers(&self, own: PathBuf) -> impl Iterator<Item = (PathBuf, bool)> {
        self.shipped_path(&own)
            .map(|shipped| (shipped, false))
            .into_iter()
            .chain([(own, true)])
    }
}
