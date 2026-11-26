//! A published release recipe can assemble without its checked source product.
use super::{Duration, Meter, NativeOptions, Output, Path, Publication, ReachableOutput};
use crate::reach::ReachCounters;
use crate::request_record::{self, Answer, Value};
use crate::{CheckProduct, GapStop, NativeStop, Reachable};
use hirsel_core_ids::ContentHash;

/// Local loading still parses disk input under CA4's decoder. Size limits
/// follow the process allowance. Bound recursive decoder stack use explicitly.
pub(crate) fn local_admission_limits() -> crate::code_admission::AdmissionLimits {
    crate::code_admission::AdmissionLimits {
        recipe_bytes: usize::MAX,
        closure_objects: usize::MAX,
        object_bytes: u64::MAX,
        decoded_bytes: usize::MAX,
        members: usize::MAX,
        edges: usize::MAX,
        nesting: 512,
        outstanding: 0,
        timeout: None,
    }
}

pub(super) struct Report {
    root: String,
    files: usize,
    checked_definitions: usize,
    rechecked: usize,
    source_bytes: usize,
    bodies: usize,
    declarations: usize,
    first_stops: Vec<GapStop>,
    record: Option<(ContentHash, Value)>,
}

impl Report {
    pub(super) fn new(
        product: &CheckProduct,
        reached: &mut Reachable<'_>,
        key: Option<ContentHash>,
        json: bool,
    ) -> Self {
        let record = key.and_then(|key| {
            let value = Value::of(Answer {
                product,
                diagnostics: &[],
                incomplete: &[],
                resources: &[],
                reach: Some(reached),
                native: None,
            })?;
            Some((key, value))
        });
        Self {
            root: if json {
                product.root.to_string()
            } else {
                String::new()
            },
            files: product.files.len(),
            checked_definitions: product.checked_definitions(),
            rechecked: product.rechecked.len(),
            source_bytes: product.bytes,
            bodies: reached.bodies,
            declarations: reached.declarations,
            first_stops: std::mem::take(&mut reached.first_stops),
            record,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble(
        &mut self,
        publication: Publication,
        options: &NativeOptions,
        counters: ReachCounters,
        library: Option<&Path>,
        manifest: Option<&Path>,
        grant_lines: bool,
        place_timeout: Duration,
        meter: &mut Meter,
    ) -> Result<Output<'_>, Box<dyn std::error::Error>> {
        let root = publication.root;
        let recipe = (manifest.is_some() || grant_lines).then(|| publication.recipe.clone());
        let built = publication.assemble_with(options, counters, library);
        if let (Ok(built), Some(path), Some(recipe)) = (&built, manifest, &recipe) {
            crate::release_manifest::write(path, &built.executable, root, recipe, place_timeout)?;
        }
        if let (true, Ok(_), Some(recipe)) = (grant_lines, &built, &recipe) {
            for line in crate::native::recipe_build::foreign_grant_lines(recipe, root) {
                eprintln!("{line}");
            }
        }
        let (native, undelivered, stopped) = match built {
            Ok(build) => (Some(build), None, None),
            Err(NativeStop::Output { product, error }) => (
                None,
                Some(*product),
                Some(crate::executable::output_stop(&error)),
            ),
            Err(error) => {
                if let NativeStop::ForeignRecording { site, .. } = &error {
                    eprintln!("{site}: recording refusal site");
                }
                (None, None, Some(error.to_string()))
            }
        };
        let build = native.as_ref().or(undelivered.as_ref());
        if let Some(build) = build {
            meter.finish(
                if build.compiled == 0 && !build.linked && build.reach.std_lowered == 0 {
                    "build store/hit"
                } else {
                    "build store/miss"
                },
            );
            if let Some((key, value)) = self.record.take() {
                request_record::publish(&options.cas, key, &value.with_native(Some(build)))?;
                meter.finish("request record/publish");
            }
        }
        let mut census = std::collections::BTreeMap::new();
        for stop in &self.first_stops {
            *census.entry(stop.construct.as_str()).or_insert(0) += 1;
        }
        Ok(Output {
            accepted: true,
            root: std::mem::take(&mut self.root),
            files: self.files,
            checked_definitions: self.checked_definitions,
            rechecked: self.rechecked,
            source_bytes: self.source_bytes,
            diagnostics: Vec::new(),
            incomplete: Vec::new(),
            resources: Vec::new(),
            image: None,
            native,
            stopped,
            reachable: Some(ReachableOutput {
                bodies: self.bodies,
                declarations: self.declarations,
                census,
                first_stops: &self.first_stops,
            }),
        })
    }
}
