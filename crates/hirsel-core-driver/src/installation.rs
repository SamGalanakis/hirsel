//! Installed selection is a small relocatable configuration, published last.
//! Package bytes retain the ordinary image/store format and cache-miss rules.
use crate::{CoreImage, Meter, PackageProduct};
use hirsel_core_ids::ContentHash;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const FILE: &str = "hirsel-core-toolchain.json";
#[derive(Default, Serialize, Deserialize)]
struct Registration {
    version: u32,
    products: BTreeMap<String, PathBuf>,
    runtimes: BTreeMap<String, PathBuf>,
}
pub struct Installed {
    pub root: PathBuf,
    registration: Registration,
}
impl Installed {
    fn at(root: &Path) -> io::Result<Option<Self>> {
        let path = root.join(FILE);
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let registration: Registration =
            serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if registration.version != 1 {
            return Err(io::Error::other(
                "unsupported core toolchain registration version",
            ));
        }
        Ok(Some(Self {
            root: root.to_owned(),
            registration,
        }))
    }
    pub fn discover(executable: &Path) -> io::Result<Option<Self>> {
        Self::at(
            executable
                .parent()
                .ok_or_else(|| io::Error::other("compiler has no installation directory"))?,
        )
    }
    pub fn cache(&self) -> PathBuf {
        self.root.join("core-packages")
    }
    pub fn runtime(&self, target: &str) -> Option<PathBuf> {
        self.registration
            .runtimes
            .get(target)
            .map(|path| self.root.join(path))
    }
    /// Open a registered library product by its package identity. A registered
    /// path that is missing or damaged is an installation error; callers must
    /// not silently fall back to source for a named installed product.
    pub fn product(&self, name: &str) -> io::Result<Option<std::sync::Arc<CoreImage>>> {
        let Some(path) = self.registration.products.get(name) else {
            return Ok(None);
        };
        let directory = self.root.join(path);
        let image = CoreImage::open(&directory)?.ok_or_else(|| {
            io::Error::other(format!(
                "installed {name} product is missing or damaged: {}",
                directory.display()
            ))
        })?;
        let has_package = image
            .directory()?
            .inputs
            .packages
            .iter()
            .any(|package| package.package == crate::package(name));
        if !has_package {
            return Err(io::Error::other(format!(
                "installed {name} product has the wrong package identity"
            )));
        }
        Ok(Some(image))
    }
}
/// Publish the produced image and optional runtime selection, not a second
/// representation of the image. A failed publication leaves the old selection.
pub fn register(
    root: &Path,
    name: &str,
    image: &CoreImage,
    runtime: Option<(&Path, &str)>,
) -> io::Result<()> {
    let root = crate::product::normalized(root);
    std::fs::create_dir_all(&root)?;
    let mut registration = Installed::at(&root)?
        .map_or_else(Registration::default, |installed| installed.registration);
    registration.version = 1;
    let image_path = crate::product::normalized(&image.store_root());
    registration.products.insert(
        name.to_owned(),
        image_path
            .strip_prefix(&root)
            .map_err(io::Error::other)?
            .to_owned(),
    );
    if let Some((runtime, target)) = runtime {
        if !["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"].contains(&target) {
            return Err(io::Error::other("unsupported installed runtime target"));
        }
        let bytes = std::fs::read(runtime)?;
        let hash = hex(ContentHash::of_bytes(&bytes));
        let relative = Path::new("runtime")
            .join(target)
            .join(hash)
            .join("libhirsel_core_rt.a");
        let path = root.join(&relative);
        let parent = path.parent().expect("runtime path has a parent");
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&bytes)?;
        file.persist(path)?;
        registration.runtimes.insert(target.to_owned(), relative);
    }
    let mut file = tempfile::NamedTempFile::new_in(&root)?;
    serde_json::to_writer_pretty(&mut file, &registration).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.persist(root.join(FILE))?;
    Ok(())
}
fn hex(hash: ContentHash) -> String {
    hash.0.iter().map(|b| format!("{b:02x}")).collect()
}
#[derive(Serialize)]
pub struct ProductEnvironment {
    pub directory: PathBuf,
    pub image: Option<String>,
    pub registered: bool,
}
#[derive(Serialize)]
pub struct Environment {
    pub executable: PathBuf,
    pub installed_root: Option<PathBuf>,
    pub package_cache: PathBuf,
    pub products: BTreeMap<&'static str, ProductEnvironment>,
    pub target: String,
    pub runtime: Option<PathBuf>,
}
pub fn environment(
    executable: &Path,
    installed: Option<&Installed>,
    cache: &Path,
    target: &str,
    runtime: Option<&Path>,
) -> io::Result<Environment> {
    let mut products = BTreeMap::new();
    for name in ["core", "std"] {
        let directory = PackageProduct::cache_directory(cache, name == "std")?;
        let image = if directory.is_dir() {
            CoreImage::open(&directory)?.map(|image| hex(image.identity))
        } else {
            None
        };
        let registered = installed.is_some_and(|installed| {
            installed
                .registration
                .products
                .get(name)
                .is_some_and(|path| {
                    crate::product::normalized(&installed.root.join(path))
                        == crate::product::normalized(&directory)
                })
        });
        products.insert(
            name,
            ProductEnvironment {
                directory,
                image,
                registered,
            },
        );
    }
    Ok(Environment {
        executable: executable.to_owned(),
        installed_root: installed.map(|i| i.root.clone()),
        package_cache: crate::product::normalized(cache),
        products,
        target: target.to_owned(),
        runtime: runtime.map(Path::to_owned),
    })
}
/// The same producer used by the CLI and the future distribution assembler.
pub fn publish(
    root: &Path,
    std: bool,
    runtime: Option<(&Path, &str)>,
    meter: &mut Meter,
) -> io::Result<PackageProduct> {
    let cache = crate::product::normalized(root).join("core-packages");
    let product = if std {
        PackageProduct::bundled_std(&cache, meter)?
    } else {
        PackageProduct::bundled(&cache, meter)?
    };
    register(
        root,
        if std { "std" } else { "core" },
        &product.image,
        runtime,
    )?;
    Ok(product)
}
