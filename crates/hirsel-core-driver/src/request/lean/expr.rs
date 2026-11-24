use super::*;

impl<'a, 'm> Fx<'a, 'm> {
    pub(super) fn state_param(&self) -> Option<&str> {
        match &self.f.prog {
            Some(State::Param(p)) => Some(p),
            _ => None,
        }
    }

    pub(super) fn is_tx(&self, name: &str) -> bool {
        self.f.params.iter().any(|p| p.tx && p.name == name)
    }

    pub(super) fn resolve(&mut self, call: NodeId) -> R<Resolved> {
        let s = self.s;
        let callee = s.kid(call, 0)?;
        let name = s.text(callee);
        let span = s.span(call);
        if s.kind(callee) == K::Path && !name.contains('.') {
            if let Some((lean, arity, _)) = self.m.ctors.get(name) {
                if self
                    .join
                    .at(span)
                    .any(|i| matches!(i, Inst::Call { .. } | Inst::BuiltinUse { .. }))
                {
                    bail!(
                        "{}: `{name}` is a constructor here but the IR calls something at its span",
                        self.f.name
                    );
                }
                self.stats.ctors += 1;
                return Ok(Resolved::Ctor(lean.clone(), *arity));
            }
        }
        let insts: Vec<&Inst> = self.join.at(span).collect();
        let builtins: Vec<String> = insts
            .iter()
            .filter_map(|i| match i {
                Inst::BuiltinUse { definition } => Some(definition.to_string()),
                _ => None,
            })
            .collect();
        let directs: Vec<String> = insts
            .iter()
            .filter_map(|i| self.join.direct_path(i))
            .collect();
        let last = name.rsplit('.').next().unwrap_or(name);
        if let [b] = builtins.as_slice() {
            let Some((path, _)) = BUILTINS.iter().find(|(p, _)| p == b) else {
                bail!(
                    "{}: builtin {b} at `{}` has no Lean binding",
                    self.f.name,
                    s.text(call)
                );
            };
            if !b.ends_with(&format!("/{last}")) {
                bail!("{}: `{name}` joins builtin {b}", self.f.name);
            }
            for helper in &directs {
                if !matches!(
                    (b.as_str(), helper.as_str()),
                    ("core@1/list/push", "core@1/list/grow")
                        | ("core@1/core/clone", "core@1/core/$clone")
                ) {
                    bail!(
                        "{}: builtin {b} has an unmodelled helper {helper}",
                        self.f.name
                    );
                }
            }
            self.stats.builtins_joined += 1;
            self.stats.helpers += directs.len();
            self.joined_ir.insert(span);
            return Ok(Resolved::Builtin(path));
        }
        if !builtins.is_empty() {
            bail!("{}: several builtins at `{}`", self.f.name, s.text(call));
        }
        match directs.as_slice() {
            [d] => {
                if !d.ends_with(&format!("/{last}")) {
                    bail!("{}: `{name}` joins {d}", self.f.name);
                }
                self.stats.calls_joined += 1;
                self.joined_ir.insert(span);
                if let Some(rest) = d.strip_prefix(&format!("{}/", self.m.prefix)) {
                    if self.m.fns.contains_key(rest) {
                        return Ok(Resolved::Extracted(rest.to_owned()));
                    }
                    bail!(
                        "{}: calls {d}, which is not in the extracted set",
                        self.f.name
                    );
                }
                if let Some(p) = PRELUDE_FNS.iter().find(|p| p.path == d) {
                    return Ok(Resolved::Prelude(p));
                }
                bail!(
                    "{}: calls {d}, which is neither extracted nor bound by the prelude",
                    self.f.name
                );
            }
            [] => {
                self.stats
                    .unresolved_syntax
                    .push(format!("{}: `{}`", self.f.name, s.text(call)));
                bail!(
                    "{}: no IR call at the span of `{}`",
                    self.f.name,
                    s.text(call)
                );
            }
            _ => bail!(
                "{}: several calls at the span of `{}`",
                self.f.name,
                s.text(call)
            ),
        }
    }

    // ---- expressions ----

    pub(super) fn path(&self, n: NodeId) -> R<String> {
        let text = self.s.text(n);
        let segs: Vec<&str> = text.split('.').collect();
        let head = segs[0];
        let mut out = if Some(head) == self.state_param() {
            "(← Hirsel.get)".to_owned()
        } else if segs.len() == 1 && self.m.ctors.contains_key(head) {
            let (lean, arity, _) = &self.m.ctors[head];
            if *arity != Some(0) {
                bail!("{}: constructor `{head}` used as a value", self.f.name);
            }
            return Ok(lean.clone());
        } else if self.is_tx(head) {
            bail!("{}: the transaction `{head}` used as a value", self.f.name);
        } else {
            mangle(head)
        };
        for seg in &segs[1..] {
            out.push('.');
            out.push_str(&mangle(seg));
        }
        Ok(out)
    }

    pub(super) fn literal(&mut self, n: NodeId) -> R<String> {
        let t = self.s.text(n);
        if t == "true" || t == "false" {
            return Ok(t.to_owned());
        }
        let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
        let suffix = &t[digits.len()..];
        let value: u64 = digits.parse().map_err(|_| format!("bad literal {t}"))?;
        let mut width = match suffix {
            "u8" => Some(8),
            "u16" => Some(16),
            "u32" => Some(32),
            "u64" => Some(64),
            "" => None,
            _ => bail!("{}: unsupported literal `{t}`", self.f.name),
        };
        let mut anchored = false;
        for inst in self.join.at(self.s.span(n)) {
            if let Inst::Const { result, bits } = inst {
                let got = self.join.body.values[result.index()].int();
                let Some((false, got)) = got else {
                    bail!(
                        "{}: integer literal `{t}` has incompatible IR type {got:?}",
                        self.f.name
                    )
                };
                if *bits != value || width.is_some_and(|w| w != got) {
                    bail!(
                        "{}: literal `{t}` disagrees with IR Const {bits} of width {got}",
                        self.f.name
                    );
                }
                width = Some(got);
                anchored = true;
            }
        }
        let width =
            width.ok_or_else(|| format!("{}: no IR width for literal `{t}`", self.f.name))?;
        if anchored {
            self.stats.literals_joined += 1
        } else {
            self.stats.literals_unanchored += 1
        }
        let lean = format!("UInt{width}");
        Ok(format!("({value} : {lean})"))
    }

    /// An expression. Calls of `Hirsel.Prog` functions are lifted with `(← …)`.
    pub(super) fn expr(&mut self, n: NodeId) -> R<String> {
        let s = self.s;
        Ok(match s.kind(n) {
            K::Path => self.path(n)?,
            K::Literal => self.literal(n)?,
            K::Unit => "()".into(),
            K::Tuple => {
                let parts = s
                    .kids(n)
                    .iter()
                    .map(|&x| self.expr(x))
                    .collect::<R<Vec<_>>>()?;
                format!("({})", parts.join(", "))
            }
            K::Index if self.has_causes && self.f.prog.is_none() => {
                let xs = atom(self.expr(s.kid(n, 0)?)?);
                let i = atom(self.expr(s.kid(n, 1)?)?);
                self.joined_failures.insert(s.span(n));
                format!("(← Hirsel.getIndex {xs} {i})")
            }
            K::Group => format!("({})", self.expr(s.kid(n, 0)?)?),
            K::Unary => {
                let inner = s.kid(n, 0)?;
                match s.tok(n) {
                    "mut" | "take" => self.expr(inner)?,
                    "!" => {
                        let e = self.expr(inner)?;
                        format!("!{}", atom(e))
                    }
                    op => bail!("{}: unsupported unary `{op}`", self.f.name),
                }
            }
            K::Binary => self.binary(n, false)?,
            K::Projection => {
                let e = self.expr(s.kid(n, 0)?)?;
                format!("{}.{}", atom(e), mangle(s.tok(n)))
            }
            K::Call => self.call(n, false)?,
            K::Try => self.try_expr(n)?,
            K::If => {
                let kids = s.kids(n);
                if kids.len() != 3 {
                    bail!("{}: an `if` value needs an else", self.f.name);
                }
                let c = self.cond(kids[0])?;
                let a = self.block_value(kids[1])?;
                let b = self.block_value(kids[2])?;
                if a.contains('←') || b.contains('←') {
                    bail!("{}: an effect inside an `if` value", self.f.name);
                }
                format!("if {c} then {a} else {b}")
            }
            other => bail!(
                "{}: unsupported expression {other:?} `{}`",
                self.f.name,
                s.text(n)
            ),
        })
    }

    pub(super) fn block_value(&mut self, n: NodeId) -> R<String> {
        if self.s.kind(n) != K::Block {
            return self.expr(n);
        }
        match self.s.kids(n) {
            [e] if self.s.kind(*e) == K::ExpressionStatement => self.expr(self.s.kid(*e, 0)?),
            _ => bail!("{}: a block value with statements", self.f.name),
        }
    }

    pub(super) fn binary(&mut self, n: NodeId, prop: bool) -> R<String> {
        let s = self.s;
        let op = s.tok(n);
        let operand = |fx: &mut Self, m: NodeId| -> R<String> {
            let e = fx.expr(m)?;
            Ok(if fx.s.kind(m) == K::Binary {
                format!("({e})")
            } else {
                e
            })
        };
        let l = operand(self, s.kid(n, 0)?)?;
        let r = operand(self, s.kid(n, 1)?)?;
        let span = s.span(n);
        let compare = self.join.at(span).any(|i| matches!(i, Inst::Compare { .. }))
            || self.join.at(span).any(|i| matches!(i, Inst::Call { call } if matches!(self.join.body.calls[call.index()].callee, Callee::Runtime(RtOp::CompareBytes))));
        Ok(match op {
            "&&" | "||" => {
                if r.contains('←') {
                    if !self.has_causes || self.f.prog.is_some() {
                        bail!("{}: effect in the right operand of `{op}`", self.f.name);
                    }
                    let method = if op == "&&" { "andAlso" } else { "orElse" };
                    return Ok(format!(
                        "(← Hirsel.{method} (do return {l}) (do return {r}))"
                    ));
                }
                format!("{l} {op} {r}")
            }
            "==" | "!=" => {
                if compare {
                    self.stats.compares_joined += 1;
                    self.joined_ir.insert(span);
                }
                format!("{l} {op} {r}")
            }
            "<" | "<=" | ">" | ">=" => {
                if compare {
                    self.stats.compares_joined += 1;
                }
                if prop {
                    format!("{l} {op} {r}")
                } else {
                    format!("decide ({l} {op} {r})")
                }
            }
            "+" if self.has_causes && self.f.prog.is_none() => {
                let checked = self.join.at(span).any(|inst| matches!(inst, Inst::Binary { op: hirsel_core_ir::BinaryOp::Add, left, .. } if self.join.body.values[left.index()].int().is_some_and(|(signed, _)| !signed)));
                if !checked {
                    bail!(
                        "{}: no checked unsigned addition at `{}`",
                        self.f.name,
                        s.text(n)
                    );
                }
                self.joined_failures.insert(span);
                format!("(← Hirsel.checkedAdd {} {})", atom(l), atom(r))
            }
            _ => bail!("{}: unsupported operator `{op}`", self.f.name),
        })
    }

    /// An `if` condition: comparisons stay propositions.
    pub(super) fn cond(&mut self, n: NodeId) -> R<String> {
        let s = self.s;
        let c = match s.kind(n) {
            K::Binary if matches!(s.tok(n), "<" | "<=" | ">" | ">=") => self.binary(n, true)?,
            K::ConditionLet => {
                let pat = self.pattern(s.kid(n, 0)?)?;
                let e = self.expr(s.kid(n, 1)?)?;
                format!("let {pat} := {e}")
            }
            _ => self.expr(n)?,
        };
        Ok(c)
    }

    /// Arguments of a call, minus the transaction (the state).
    pub(super) fn args(&mut self, call: NodeId, tx_args: &[usize]) -> R<Vec<NodeId>> {
        let s = self.s;
        let mut out = Vec::new();
        for (i, &a) in s.kids(call)[1..].iter().enumerate() {
            if tx_args.contains(&i) {
                let inner = if s.kind(a) == K::Unary {
                    s.kid(a, 0)?
                } else {
                    a
                };
                if !self.is_tx(s.text(inner)) {
                    bail!(
                        "{}: argument {i} of `{}` is not the transaction",
                        self.f.name,
                        s.text(call)
                    );
                }
                continue;
            }
            out.push(a);
        }
        Ok(out)
    }

    pub(super) fn arg_list(&mut self, args: &[NodeId]) -> R<String> {
        let mut out = String::new();
        for &a in args {
            let e = self.expr(a)?;
            out.push(' ');
            out.push_str(&atom(e));
        }
        Ok(out)
    }

    /// A call. `action` is true when it is a `do` element, so a `Prog` call is not lifted.
    pub(super) fn call(&mut self, n: NodeId, action: bool) -> R<String> {
        let s = self.s;
        let lift = |e: String| if action { e } else { format!("(← {e})") };
        match self.resolve(n)? {
            Resolved::Ctor(lean, arity) => {
                let kids = &s.kids(n)[1..];
                match arity {
                    None => {
                        let owner = &self.m.ctors[s.text(s.kid(n, 0)?)].2;
                        let mut fields = Vec::new();
                        for &k in kids {
                            if s.kind(k) != K::LabelledArgument {
                                bail!("{}: record `{owner}` built without labels", self.f.name);
                            }
                            fields.push(format!(
                                "{} := {}",
                                mangle(s.tok(k)),
                                self.expr(s.kid(k, 0)?)?
                            ));
                        }
                        let count = self.m.types[owner].generics.len();
                        let placeholders = " _".repeat(count);
                        Ok(format!(
                            "{{ {} : {owner}{placeholders} }}",
                            fields.join(", ")
                        ))
                    }
                    Some(k) if k == kids.len() => {
                        let args = self.arg_list(kids)?;
                        Ok(format!("{lean}{args}"))
                    }
                    Some(k) => bail!("{}: `{lean}` takes {k} arguments", self.f.name),
                }
            }
            Resolved::Extracted(name) => {
                let callee = &self.m.fns[&name];
                let tx: Vec<usize> = callee
                    .params
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.tx)
                    .map(|(i, _)| i)
                    .collect();
                if let Some(State::Param(_)) = callee.prog {
                    bail!("{}: calls {name}, whose state is a parameter", self.f.name);
                }
                let args = self.args(n, &tx)?;
                let args = self.arg_list(&args)?;
                let e = format!("{}{args}", mangle(&name));
                Ok(if callee.prog.is_some() { lift(e) } else { e })
            }
            Resolved::Prelude(p) => {
                if p.mut_out.is_some() {
                    bail!(
                        "{}: `{}` updates a `mut` argument; only `?=>` statements are modelled",
                        self.f.name,
                        p.path
                    );
                }
                let args = self.args(n, p.tx_args)?;
                let args = self.arg_list(&args)?;
                let e = format!("{}{args}", p.lean);
                Ok(if p.effect { lift(e) } else { e })
            }
            Resolved::Builtin(path) => {
                let args: Vec<NodeId> = s.kids(n)[1..].to_vec();
                match path {
                    "core@1/list/new" => Ok("[]".into()),
                    "core@1/list/slice" if self.has_causes && self.f.prog.is_none() => {
                        let a = self.arg_list(&args)?;
                        self.joined_failures.insert(s.span(n));
                        Ok(format!("(← Hirsel.list.slice{a})"))
                    }
                    "core@1/list/slice" => {
                        bail!("{}: list.slice needs a cause-only body", self.f.name)
                    }
                    "core@1/core/clone" => self.expr(args[0]),
                    "core@1/list/push" => {
                        let place = if s.kind(args[0]) == K::Unary {
                            s.kid(args[0], 0)?
                        } else {
                            args[0]
                        };
                        let v = self.expr(args[1])?;
                        let text = s.text(place);
                        let segs: Vec<&str> = text.split('.').collect();
                        if Some(segs[0]) == self.state_param() && segs.len() == 2 {
                            let f = mangle(segs[1]);
                            let e = format!(
                                "Hirsel.modify fun s => {{ s with {f} := Hirsel.list.push s.{f} {} }}",
                                atom(v)
                            );
                            return Ok(lift(e));
                        }
                        bail!("{}: `list.push` on `{text}` in value position", self.f.name)
                    }
                    _ => {
                        let lean = BUILTINS.iter().find(|(p, _)| *p == path).unwrap().1;
                        let a = self.arg_list(&args)?;
                        Ok(format!("{lean}{a}"))
                    }
                }
            }
        }
    }

    /// `e?` as a value: the call throws its error itself.
    pub(super) fn try_expr(&mut self, n: NodeId) -> R<String> {
        let s = self.s;
        if s.kids(n).len() != 1 {
            bail!("{}: `?=>` is modelled only as a statement", self.f.name);
        }
        let inner = s.kid(n, 0)?;
        if s.kind(inner) != K::Call {
            bail!("{}: `?` on a non-call", self.f.name);
        }
        match self.resolve(inner)? {
            Resolved::Extracted(name) if self.m.fns[&name].prog.is_some() => {
                self.call(inner, false)
            }
            _ => bail!(
                "{}: `?` on `{}`, which is not a fallible extracted function",
                self.f.name,
                s.text(inner)
            ),
        }
    }

    // ---- patterns ----

    pub(super) fn pattern(&mut self, n: NodeId) -> R<String> {
        let s = self.s;
        Ok(match s.kind(n) {
            K::Wildcard => "_".into(),
            K::TuplePattern => {
                let parts = s
                    .kids(n)
                    .iter()
                    .map(|&x| self.pattern(x))
                    .collect::<R<Vec<_>>>()?;
                format!("({})", parts.join(", "))
            }
            K::BindPattern | K::TakePattern | K::MutPattern => {
                let name = s.tok(n);
                match self.m.ctors.get(name) {
                    Some((lean, Some(0), _)) if s.kind(n) == K::BindPattern => lean.clone(),
                    _ => mangle(name),
                }
            }
            K::ConstructorPattern => {
                let kids = s.kids(n);
                let name = s.text(kids[0]);
                let Some((lean, Some(arity), _)) = self.m.ctors.get(name).cloned() else {
                    bail!(
                        "{}: unsupported constructor pattern `{}`",
                        self.f.name,
                        s.text(n)
                    );
                };
                if arity != kids.len() - 1 {
                    bail!(
                        "{}: pattern `{}` has the wrong arity",
                        self.f.name,
                        s.text(n)
                    );
                }
                self.stats.ctors += 1;
                let mut out = lean;
                for &field in &kids[1..] {
                    if s.kind(field) != K::PatternField {
                        bail!(
                            "{}: unsupported pattern field in `{}`",
                            self.f.name,
                            s.text(n)
                        );
                    }
                    out.push(' ');
                    out.push_str(&atom(self.pattern(s.kid(field, 0)?)?));
                }
                out
            }
            other => bail!(
                "{}: unsupported pattern {other:?} `{}`",
                self.f.name,
                s.text(n)
            ),
        })
    }
}
