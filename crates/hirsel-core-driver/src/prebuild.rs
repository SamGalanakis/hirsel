//! An installation's prebuilt objects. Every program-independent instance of
//! an image's packages is emitted once, for the development engine and a
//! request's default options ([`NativeOptions::development`]), by the walk and
//! emission a program's build runs, into the image's own native store: a program's first build then finds each of them under the key it
//! computes itself. Program-independent means a plain identity (no
//! dictionaries, no closures) of a non-generic function the image checked
//! whole; the set is read from the image, never listed.
use super::*;
use crate::native::{Link, NativeOptions, NativeStop};
use hirsel_core_ir::Identity;

/// What one prebuild found and emitted.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Prebuilt {
    /// Plain instances of the image's non-generic functions.
    pub instances: usize,
    /// Of them, those whose object was compiled now, not already stored.
    pub compiled: usize,
    /// Non-generic functions skipped: a body the image refused, or one whose
    /// instance stops, which a program's build stops on too.
    pub skipped: usize,
}

impl CheckProduct {
    /// Emit every plain instance of this product's image for a development
    /// request on `target` linking `runtime` with `link`; `scratch` holds what
    /// the emission writes outside the image's store.
    pub fn prebuild(
        &self,
        target: String,
        runtime: PathBuf,
        link: Link,
        scratch: &Path,
    ) -> Result<Prebuilt, NativeStop> {
        let options = &NativeOptions::development(
            target,
            runtime,
            link,
            scratch.join("native"),
            scratch.join("program"),
        );
        let image = self
            .image
            .as_ref()
            .ok_or_else(|| NativeStop::Entry("a prebuild emits an image's instances".into()))?;
        let inputs = crate::native::native_inputs(self, options).map_err(NativeStop::Input)?;
        let mut materialized = crate::instances::InstanceSet::default();
        let mut resolver = hirsel_core_materialize::Resolver::default();
        let mut prebuilt = Prebuilt::default();
        // No prior reach record: every instance walks its own record or body.
        let no_damage = std::collections::BTreeSet::new();
        let mut instances = Vec::new();
        let directory = image.directory().map_err(NativeStop::Input)?;
        for definition in directory.definitions.keys() {
            let product = self.demand_body(definition).map_err(NativeStop::Input)?;
            if product.check.function.is_none() || product.check.template.is_some() {
                continue;
            }
            let key = Identity::plain(definition.clone());
            // A plain body reads no closure argument, so each walks alone; its
            // children are emitted as their own roots or by the program.
            match self.instance_step(
                &key,
                &mut materialized,
                inputs.records.as_ref().map(|records| {
                    let expand = crate::instances::Expand {
                        prior: None,
                        changed: &[],
                        damaged: &no_damage,
                    };
                    (records, expand)
                }),
                &mut resolver,
                &mut |_, _| {},
            ) {
                Ok(_) => instances.push(key),
                Err(_) => prebuilt.skipped += 1,
            }
        }
        let (objects, _) =
            self.emit_instances(&instances, &materialized, &inputs, None, options)?;
        prebuilt.instances = objects.len();
        prebuilt.compiled = objects.iter().filter(|o| o.compiled).count();
        Ok(prebuilt)
    }
}
