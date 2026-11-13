use super::*;
fn refusal(message: impl Into<String>) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "static dependency discovery".into(),
        message: message.into(),
    })
}
impl QueryEngine {
    pub fn discover_front_dependencies(
        &mut self,
        roots: &[DefPath],
        fronts: &[Arc<FrontBodyQueries>],
    ) -> Result<Vec<DefPath>, QueryError> {
        let mut pending: Vec<_> = roots
            .iter()
            .rev()
            .cloned()
            .map(|p| (p, BTreeMap::new()))
            .collect();
        let mut seen = BTreeSet::new();
        let mut missing = BTreeSet::new();
        while let Some((path, bindings)) = pending.pop() {
            let binding_key: Vec<_> = bindings
                .iter()
                .map(
                    |(site, actuals): (&hirsel_types::SiteRef, &hirsel_types::Actuals)| {
                        (
                            *site,
                            hirsel_types::closed_actual_key(&self.types, &actuals.arguments),
                        )
                    },
                )
                .collect();
            if !seen.insert((path.clone(), binding_key)) {
                continue;
            }
            if seen.len() > 65536 {
                return Err(refusal("instance selection expansion limit"));
            }
            if !self.declared(&path) {
                continue;
            }
            let mut product = None;
            for front in fronts {
                if let Some(output) =
                    front
                        .instance_discovery_summary(self, &path)
                        .map_err(|mut error| {
                            if let QueryError::Operational(failure) = &mut error
                                && failure.message.starts_with("stale front summary")
                                && let Some(output) = front.checked_definition(&path)
                                && let Some(key) =
                                    output.reads.first_changed(|key| self.observe(&path, key))
                            {
                                failure.message.push_str(&format!(
                                    "; changed {key:?}: {:?} -> {:?}",
                                    output.reads.get(&key),
                                    self.observe(&path, &key)
                                ));
                            }
                            error
                        })?
                {
                    product = Some(output);
                    break;
                }
            }
            let Some(product) = product else {
                if fronts.iter().any(|front| {
                    matches!(
                        front.completed_error(&path),
                        Some(QueryError::Diagnostics(_))
                    )
                }) {
                    continue;
                }
                missing.insert(path);
                continue;
            };
            let summary = &product.summary.summary;
            if bindings.is_empty() && summary.frames[0].generic {
                continue;
            }
            for call in summary.calls.iter().filter(|call| {
                call.target_frame.is_none() && call.class != CallClass::ComptimeExecuted
            }) {
                let callee = call.callee.path();
                if !self.declared(callee) {
                    continue;
                }
                let actuals = hirsel_types::close_arguments(
                    &self.types,
                    &self.memo,
                    &call.arguments,
                    &bindings,
                )
                .map_err(|e| refusal(format!("closing {callee} arguments: {e:?}")))?;
                let key = hirsel_types::closed_actual_key(&self.types, &actuals.arguments);
                if actuals.arguments.is_empty() {
                    pending.push((callee.clone(), BTreeMap::new()));
                } else if key.is_some() {
                    pending.push((callee.clone(), BTreeMap::from([(call.binder, actuals)])));
                }
            }
        }
        let pending_bodies: Vec<_> = missing.into_iter().collect();
        Ok(pending_bodies)
    }
}
