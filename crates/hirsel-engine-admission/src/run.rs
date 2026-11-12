//! Signature and code policy binding before host admission.
use hirsel_run_config::{
    AdmittedRun, BoundRun, HostAuthority, ProgramFacts, Root, RunConfig, ValidatedRun,
};
use hirsel_run_schema::{CodeReach, LaunchKind, ProgramRevision, RunRefusal};
use hirsel_syntax::Visibility;
use hirsel_types::{Signature, SignatureKind, TypeInterner, TypeKind};
use std::path::Path;

/// Read one explicit absolute record. Neither argv nor environment supplies grants.
pub fn read_run_record(path: &Path) -> Result<ValidatedRun, RunRefusal> {
    RunConfig::read_path(path)
}

/// The exact nominal roots, in parameter order, of an eligible entry whose
/// rows live in `t`.
pub fn entry_roots(
    t: &TypeInterner,
    signature: &Signature,
    launch: LaunchKind,
) -> Result<Vec<Root>, RunRefusal> {
    if launch == LaunchKind::Call {
        return callee_roots(t, signature);
    }
    let invalid = |detail: &str| RunRefusal::Invalid {
        field: "entry".into(),
        detail: detail.into(),
    };
    let SignatureKind::Function(function) = &signature.kind else {
        return Err(invalid("expected a function"));
    };
    if signature.metadata.visibility != Visibility::Public
        || !function.generics.is_empty()
        || function.incomplete
        || function.foreign.is_some()
    {
        return Err(invalid(
            "expected a public, complete, monomorphic Hirsel function",
        ));
    }
    if hirsel_types::entry::entry_return(t, function.result).is_none() {
        return Err(invalid(
            "expected Result[(), E], std@1/process.ExitCode or Result[std@1/process.ExitCode, E]",
        ));
    }
    let nominal = |definition: &hirsel_syntax::DefId, name: &str| {
        let definition = definition.path();
        definition.package_is("core", "1")
            && definition.0.modules.is_empty()
            && definition.0.name == name
    };
    let roots = function
        .parameters
        .iter()
        .map(|parameter| {
            let TypeKind::Named {
                definition,
                arguments,
            } = &t
                .get(parameter.ty)
                .ok_or_else(|| invalid("missing parameter type"))?
                .kind
            else {
                return Err(invalid("expected nominal root capabilities"));
            };
            let name = &definition.path().0.name;
            if !arguments.is_empty() || !nominal(definition, name) {
                return Err(invalid("expected nominal root capabilities"));
            }
            match name.as_str() {
                "Io" => Ok(Root::Io),
                "Args" => Ok(Root::Args),
                "Env" => Ok(Root::Env),
                "Unsafe" => Ok(Root::Unsafe),
                "Test" => Ok(Root::Test),
                _ => Err(invalid("expected nominal root capabilities")),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    if (launch == LaunchKind::Test && roots != [Root::Test])
        || (launch == LaunchKind::Main && roots.contains(&Root::Test))
    {
        return Err(invalid("parameters do not match the launch kind"));
    }
    Ok(roots)
}

/// Whether a definition has an entry's identity (RUN-3, RUN-8): a complete, non-generic,
/// non-foreign function returning `Result`, the shape of every runner callee and of every
/// entry `runner.apply` runs. Its references are what `runner.entry_id` names.
pub fn identified_entry(t: &TypeInterner, signature: &Signature) -> bool {
    let SignatureKind::Function(function) = &signature.kind else {
        return false;
    };
    function.generics.is_empty()
        && !function.incomplete
        && function.foreign.is_none()
        && matches!(
            t.get(function.result).map(|t| &t.kind),
            Some(TypeKind::Named { definition, .. })
                if definition.path().package_is("core", "1")
                    && definition.path().0.modules.is_empty()
                    && definition.path().0.name == "Result"
        )
}

/// The roots of a callee run's entry (RUN-3, RUN-10): a complete, monomorphic Hirsel
/// function of any visibility taking its argument, after the place's `Io` for the call
/// form, and returning a `Result`.
pub fn callee_roots(t: &TypeInterner, signature: &Signature) -> Result<Vec<Root>, RunRefusal> {
    let invalid = || RunRefusal::Invalid {
        field: "entry".into(),
        detail: "expected a runner entry: fn(a: A) or fn(rt: Io, a: A), returning a Result".into(),
    };
    let SignatureKind::Function(function) = &signature.kind else {
        return Err(invalid());
    };
    if !function.generics.is_empty() || function.incomplete || function.foreign.is_some() {
        return Err(invalid());
    }
    let named = |ty, name: &str| {
        matches!(
            t.get(ty).map(|t| &t.kind),
            Some(TypeKind::Named { definition, .. })
                if definition.path().package_is("core", "1")
                    && definition.path().0.modules.is_empty()
                    && definition.path().0.name == name
        )
    };
    if !named(function.result, "Result") {
        return Err(invalid());
    }
    match &function.parameters[..] {
        [_] => Ok(vec![]),
        [io, _] if named(io.ty, "Io") => Ok(vec![Root::Io]),
        _ => Err(invalid()),
    }
}

/// Select the admission kind from the entry's checked nominal signature.
pub fn entry_launch_kind(
    t: &TypeInterner,
    signature: &Signature,
) -> Result<LaunchKind, RunRefusal> {
    if entry_roots(t, signature, LaunchKind::Test).is_ok() {
        Ok(LaunchKind::Test)
    } else {
        entry_roots(t, signature, LaunchKind::Main).map(|_| LaunchKind::Main)
    }
}

/// Code selection defaults to denial. Check every binding and root before host admission.
#[allow(clippy::too_many_arguments)]
pub fn admit_run(
    t: &TypeInterner,
    signature: &Signature,
    revision: &ProgramRevision,
    launch: LaunchKind,
    requested: Option<&ValidatedRun>,
    code: CodeReach,
    host: &mut dyn HostAuthority,
    now: i64,
) -> Result<AdmittedRun, RunRefusal> {
    bind_run(t, signature, revision, launch, requested, code)?.admit(host, now)
}

/// What binding a run needs from the checked program: the revision, the selected launch
/// kind, the entry's roots for that kind, whether the signature is the revision's entry, and
/// the code reach. A program record stores the bindable form ([`EntryFacts::program_facts`]), so
/// a run it answers binds without the checked types.
#[derive(Clone, Debug)]
pub struct EntryFacts {
    revision: ProgramRevision,
    launch: LaunchKind,
    roots: Result<Vec<Root>, RunRefusal>,
    entry_matches: bool,
    code: CodeReach,
}

impl EntryFacts {
    pub fn of(
        t: &TypeInterner,
        signature: &Signature,
        revision: &ProgramRevision,
        launch: LaunchKind,
        code: CodeReach,
    ) -> Self {
        Self {
            revision: revision.clone(),
            launch,
            roots: entry_roots(t, signature, launch),
            entry_matches: signature.definition.path().to_string() == revision.entry(),
            code,
        }
    }

    /// The facts a record stored for a program whose run bound.
    pub fn recorded(facts: ProgramFacts) -> Self {
        Self {
            revision: ProgramRevision::recorded(facts.entry, facts.program),
            launch: facts.launch,
            roots: Ok(facts.roots),
            entry_matches: true,
            code: facts.code,
        }
    }

    pub fn revision(&self) -> &ProgramRevision {
        &self.revision
    }

    pub fn launch(&self) -> LaunchKind {
        self.launch
    }

    pub fn roots(&self) -> Result<Vec<Root>, RunRefusal> {
        self.roots.clone()
    }

    pub fn code(&self) -> &CodeReach {
        &self.code
    }

    /// The bindable facts, when the entry binds at all.
    pub fn program_facts(&self) -> Option<ProgramFacts> {
        let roots = self.roots.as_ref().ok()?;
        self.entry_matches.then(|| {
            ProgramFacts::of(
                &self.revision,
                self.launch,
                roots.clone(),
                self.code.clone(),
            )
        })
    }

    /// Bind the requested run (or the program's default record) to these facts.
    pub fn bind(&self, requested: Option<&ValidatedRun>) -> Result<BoundRun, RunRefusal> {
        let roots = self.roots()?;
        if !self.entry_matches {
            return Err(RunRefusal::Binding {
                field: "entry.signature".into(),
            });
        }
        let run = match requested {
            Some(run) => run.clone(),
            None => RunConfig::for_program(&self.revision, self.launch).validated()?,
        };
        run.bind(&ProgramFacts::of(
            &self.revision,
            self.launch,
            roots,
            self.code.clone(),
        ))
    }
}

/// Bind the requested run (or the program's default record) to the checked code it would
/// execute, with no host consulted: the entry's signature, binding, roots and code policy. A
/// launcher that admits the run elsewhere (a native executable) binds here only.
pub fn bind_run(
    t: &TypeInterner,
    signature: &Signature,
    revision: &ProgramRevision,
    launch: LaunchKind,
    requested: Option<&ValidatedRun>,
    code: CodeReach,
) -> Result<BoundRun, RunRefusal> {
    EntryFacts::of(t, signature, revision, launch, code).bind(requested)
}
