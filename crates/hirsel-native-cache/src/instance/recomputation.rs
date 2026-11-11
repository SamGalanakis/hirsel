//! Failure-only preservation of both native outputs, including request-local
//! content that would otherwise disappear when the refusing compiler exits.
use super::Record;
use hirsel_engine_store::{Action, Reference, ResultStore};
use hirsel_native::query_model::InstanceAddress;

pub(super) struct Mismatch<'a> {
    pub key: &'a InstanceAddress,
    pub phase: u32,
    pub action: Action,
    pub before_descriptor: Reference,
    pub after_descriptor: Reference,
    pub before: Record,
    pub after: Record,
}
impl Mismatch<'_> {
    pub fn describe(&self, store: &dyn ResultStore) -> String {
        let before = [
            self.before.0,
            self.before.1,
            self.before.2,
            self.before.3,
            self.before.4,
        ];
        let after = [
            self.after.0,
            self.after.1,
            self.after.2,
            self.after.3,
            self.after.4,
        ];
        let names = [
            "value",
            "layouts",
            "equations",
            "discovery",
            "statics-finalizers",
        ];
        let mut changed = Vec::new();
        let mut components = Vec::new();
        for ((name, before), after) in names.iter().zip(before).zip(after) {
            if before != after {
                changed.push(*name);
            }
            let old = store.get_content(before);
            let new = store.get_content(after);
            let difference = old.as_ref().zip(new.as_ref()).and_then(|(old, new)| {
                let offset = old
                    .iter()
                    .zip(new.iter())
                    .position(|(a, b)| a != b)
                    .or_else(|| (old.len() != new.len()).then_some(old.len().min(new.len())))?;
                let start = offset.saturating_sub(32);
                Some(serde_json::json!({
                    "offset": offset,
                    "excerpt_start": start,
                    "before": &old[start..old.len().min(offset + 32)],
                    "after": &new[start..new.len().min(offset + 32)],
                }))
            });
            components.push(serde_json::json!({
                "name": name, "before": before, "after": after,
                "before_bytes": old.as_ref().map(|b| b.len()),
                "after_bytes": new.as_ref().map(|b| b.len()),
                "first_difference": difference,
            }));
        }
        let mut evidence = serde_json::json!({
            "schema": 1,
            "compiler": hirsel_compiler_identity::COMPILER_IDENTITY.as_slice(),
            "phase": self.phase,
            "definition": self.key.instance.definition(),
            "body": self.key.instance.body(),
            "instance_encoding": self.key.instance.encoding(),
            "target": self.key.target_abi,
            "policy": self.key.policy,
            "address": self.key.address(self.phase).0,
            "action": self.action.0,
            "before_descriptor": self.before_descriptor,
            "after_descriptor": self.after_descriptor,
            "changed": changed,
            "components": components,
            "directory": null,
            "capture_error": null,
        });
        let capture = (|| -> Result<(), String> {
            let mut builder = tempfile::Builder::new();
            builder.prefix("hirsel-instance-recomputation-");
            let directory = match std::env::var_os("HIRSEL_INSTANCE_RECOMPUTATION_DIR") {
                Some(root) => {
                    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
                    builder.tempdir_in(root)
                }
                None => builder.tempdir(),
            }
            .map_err(|e| e.to_string())?
            .keep();
            evidence["directory"] = serde_json::json!(directory.to_string_lossy());
            for ((name, before), after) in names.iter().zip(before).zip(after) {
                for (side, reference) in [("before", before), ("after", after)] {
                    let bytes = store
                        .get_content(reference)
                        .ok_or_else(|| format!("missing {side} {name} content {reference:?}"))?;
                    std::fs::write(directory.join(format!("{side}-{name}.bin")), &*bytes)
                        .map_err(|e| e.to_string())?;
                }
            }
            for (side, record) in [("before", self.before), ("after", self.after)] {
                std::fs::write(
                    directory.join(format!("{side}-descriptor.json")),
                    serde_json::to_vec(&record).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
            }
            std::fs::write(
                directory.join("evidence.json"),
                serde_json::to_vec(&evidence).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })();
        if let Err(error) = capture {
            evidence["capture_error"] = serde_json::json!(error);
        }
        format!(
            "instance recomputation changed output under unchanged complete action; evidence: {evidence}"
        )
    }
}
