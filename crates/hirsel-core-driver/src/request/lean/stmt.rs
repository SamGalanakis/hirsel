use super::*;

impl Fx<'_, '_> {
    // ---- statements ----

    pub(super) fn block(&mut self, out: &mut String, n: NodeId, ind: usize, tail: Tail) -> R<()> {
        let kids = self.s.kids(n);
        if kids.is_empty() {
            writeln!(out, "{}pure ()", pad(ind)).unwrap();
            return Ok(());
        }
        let last = kids.len() - 1;
        for (i, &st) in kids.iter().enumerate() {
            let t = if i == last { tail } else { Tail::Unit };
            self.stmt(out, st, ind, t, i == last)?;
        }
        Ok(())
    }

    pub(super) fn stmt(
        &mut self,
        out: &mut String,
        n: NodeId,
        ind: usize,
        tail: Tail,
        last: bool,
    ) -> R<()> {
        let s = self.s;
        let p = pad(ind);
        match s.kind(n) {
            K::Let | K::LetMut => {
                let kids = s.kids(n);
                let pat = kids[0];
                let (ty, value, els) = match kids.len() {
                    2 => (None, kids[1], None),
                    3 if s.kind(kids[2]) == K::Block && s.kind(pat) == K::ConstructorPattern => {
                        (None, kids[1], Some(kids[2]))
                    }
                    3 => (Some(kids[1]), kids[2], None),
                    _ => bail!("{}: unsupported let `{}`", self.f.name, s.text(n)),
                };
                let kw = if s.kind(n) == K::LetMut {
                    "let mut"
                } else {
                    "let"
                };
                if s.kind(n) == K::LetMut {
                    self.muts.insert(s.tok(pat).to_owned());
                }
                let pat_s = self.pattern(pat)?;
                let ty_s = match ty {
                    Some(t) => format!(" : {}", self.m.ty(s, t, &self.f.generics)?),
                    None => String::new(),
                };
                if s.kind(value) == K::Match {
                    let m = self.match_expr(value, ind + 2)?;
                    writeln!(out, "{p}{kw} {pat_s}{ty_s} := {m}").unwrap();
                    return Ok(());
                }
                let v = self.expr(value)?;
                if let Some(els) = els {
                    let e = self.else_throw(els)?;
                    writeln!(out, "{p}{kw} {pat_s}{ty_s} := {v} | {e}").unwrap();
                } else if let Some(inner) = v
                    .strip_prefix("(← ")
                    .and_then(|r| r.strip_suffix(')'))
                    .filter(|_| balanced_inside(&v))
                {
                    writeln!(out, "{p}{kw} {pat_s}{ty_s} ← {inner}").unwrap();
                } else {
                    writeln!(out, "{p}{kw} {pat_s}{ty_s} := {v}").unwrap();
                }
            }
            K::Assign => {
                if s.tok(n) != "=" {
                    bail!("{}: compound assignment `{}`", self.f.name, s.text(n));
                }
                let lhs = s.text(s.kid(n, 0)?);
                let rhs = self.expr(s.kid(n, 1)?)?;
                let segs: Vec<&str> = lhs.split('.').collect();
                if Some(segs[0]) == self.state_param() {
                    bail!("{}: assignment to the state `{lhs}`", self.f.name);
                }
                if !self.muts.contains(segs[0]) {
                    bail!(
                        "{}: assignment to `{lhs}`, which is not a `let mut` local",
                        self.f.name
                    );
                }
                writeln!(out, "{p}{} := {}", mangle(segs[0]), update(&segs, &rhs)).unwrap();
            }
            K::ExpressionStatement => {
                let e = s.kid(n, 0)?;
                self.expr_stmt(out, e, ind, if last { tail } else { Tail::Unit })?;
            }
            other => bail!(
                "{}: unsupported statement {other:?} `{}`",
                self.f.name,
                s.text(n)
            ),
        }
        Ok(())
    }

    /// The `else` block of `let P = e else { return Error(x) }`.
    pub(super) fn else_throw(&mut self, n: NodeId) -> R<String> {
        let s = self.s;
        if let [st] = s.kids(n) {
            let e = s.kid(*st, 0)?;
            if s.kind(e) == K::Return {
                let v = s.kid(e, 0)?;
                if let Some(x) = self.result_ctor(v, "Error")? {
                    return Ok(format!("throw {}", atom(x)));
                }
            }
        }
        bail!(
            "{}: a `let … else` whose else is not `return Error(…)`",
            self.f.name
        )
    }

    /// The payload of `Ok(x)`/`Error(x)` when `n` is that constructor call.
    pub(super) fn result_ctor(&mut self, n: NodeId, which: &str) -> R<Option<String>> {
        let s = self.s;
        if s.kind(n) == K::Call && s.text(s.kid(n, 0)?) == which && s.kids(n).len() == 2 {
            self.stats.ctors += 1;
            return Ok(Some(self.expr(s.kid(n, 1)?)?));
        }
        Ok(None)
    }

    pub(super) fn expr_stmt(
        &mut self,
        out: &mut String,
        e: NodeId,
        ind: usize,
        tail: Tail,
    ) -> R<()> {
        let s = self.s;
        let p = pad(ind);
        match s.kind(e) {
            K::If => return self.if_stmt(out, e, ind, tail),
            K::Match => return self.match_stmt(out, e, ind, tail),
            K::While => return self.while_stmt(out, e, ind),
            K::For => {
                let pat = self.pattern(s.kid(e, 0)?)?;
                let iterable = s.kid(e, 1)?;
                let xs = if s.kind(iterable) == K::Binary && s.tok(iterable) == ".." {
                    let lo = self.expr(s.kid(iterable, 0)?)?;
                    let hi = self.expr(s.kid(iterable, 1)?)?;
                    format!("Hirsel.range {} {}", atom(lo), atom(hi))
                } else {
                    self.expr(iterable)?
                };
                writeln!(out, "{p}for {pat} in {xs} do").unwrap();
                return self.block(out, s.kid(e, 2)?, ind + 2, Tail::Unit);
            }
            K::Return => {
                let v = s.kid(e, 0)?;
                let line = self.ret(v)?;
                writeln!(out, "{p}{line}").unwrap();
                return Ok(());
            }
            _ => {}
        }
        if tail == Tail::Result {
            if let Some(x) = self.result_ctor(e, "Ok")? {
                writeln!(out, "{p}pure {}", atom(x)).unwrap();
                return Ok(());
            }
            if let Some(x) = self.result_ctor(e, "Error")? {
                writeln!(out, "{p}throw {}", atom(x)).unwrap();
                return Ok(());
            }
        }
        if tail == Tail::Pure {
            let v = self.expr(e)?;
            writeln!(out, "{p}pure {}", atom(v)).unwrap();
            return Ok(());
        }
        match s.kind(e) {
            K::Call => {
                if let Resolved::Builtin("core@1/list/push") = self.resolve(e)? {
                    let place = s.kid(s.kid(e, 1)?, 0)?;
                    let segs: Vec<&str> = s.text(place).split('.').collect();
                    if Some(segs[0]) != self.state_param() {
                        let v = self.expr(s.kid(e, 2)?)?;
                        let cur = segs.iter().map(|x| mangle(x)).collect::<Vec<_>>().join(".");
                        let rhs = format!("Hirsel.list.push {cur} {}", atom(v));
                        writeln!(out, "{p}{} := {}", mangle(segs[0]), update(&segs, &rhs)).unwrap();
                        return Ok(());
                    }
                }
                let c = self.call(e, true)?;
                if tail == Tail::Result && !self.is_prog_call(e)? {
                    bail!(
                        "{}: tail `{}` is not a fallible call",
                        self.f.name,
                        s.text(e)
                    );
                }
                writeln!(out, "{p}{c}").unwrap();
            }
            K::Try if s.kids(e).len() == 2 => self.map_error_stmt(out, e, ind)?,
            K::Try => {
                let inner = s.kid(e, 0)?;
                let v = self.try_expr(e)?;
                let _ = inner;
                let action = v
                    .strip_prefix("(← ")
                    .and_then(|r| r.strip_suffix(')'))
                    .unwrap_or(&v)
                    .to_owned();
                writeln!(out, "{p}{action}").unwrap();
            }
            other => bail!(
                "{}: unsupported statement expression {other:?} `{}`",
                self.f.name,
                s.text(e)
            ),
        }
        Ok(())
    }

    pub(super) fn is_prog_call(&mut self, call: NodeId) -> R<bool> {
        Ok(match self.resolve(call)? {
            Resolved::Extracted(name) => self.m.fns[&name].prog.is_some(),
            _ => false,
        })
    }

    /// `return v`.
    pub(super) fn ret(&mut self, v: NodeId) -> R<String> {
        if self.f.prog.is_some() {
            if let Some(x) = self.result_ctor(v, "Error")? {
                return Ok(format!("throw {}", atom(x)));
            }
            if let Some(x) = self.result_ctor(v, "Ok")? {
                return Ok(format!("return {}", atom(x)));
            }
            if self.s.kind(v) == K::Call && self.is_prog_call(v)? {
                let c = self.call(v, false)?;
                return Ok(format!("return {c}"));
            }
            bail!("{}: unsupported return `{}`", self.f.name, self.s.text(v));
        }
        let x = self.expr(v)?;
        Ok(format!("return {x}"))
    }

    pub(super) fn if_stmt(&mut self, out: &mut String, n: NodeId, ind: usize, tail: Tail) -> R<()> {
        let s = self.s;
        let p = pad(ind);
        let kids = s.kids(n);
        let c = self.cond(kids[0])?;
        writeln!(out, "{p}if {c} then").unwrap();
        self.block(out, kids[1], ind + 2, tail)?;
        if let Some(&els) = kids.get(2) {
            writeln!(out, "{p}else").unwrap();
            if s.kind(els) == K::If {
                self.if_stmt(out, els, ind + 2, tail)?;
            } else {
                self.block(out, els, ind + 2, tail)?;
            }
        } else if tail != Tail::Unit {
            bail!("{}: a value `if` without else", self.f.name);
        }
        Ok(())
    }

    pub(super) fn scrutinee(&mut self, n: NodeId) -> R<String> {
        let s = self.s;
        if s.kind(n) == K::Try {
            return self.try_expr(n);
        }
        self.expr(n)
    }

    pub(super) fn match_stmt(
        &mut self,
        out: &mut String,
        n: NodeId,
        ind: usize,
        tail: Tail,
    ) -> R<()> {
        let s = self.s;
        let p = pad(ind);
        let kids = s.kids(n);
        let sc = self.scrutinee(kids[0])?;
        writeln!(out, "{p}match {sc} with").unwrap();
        for &arm in &kids[1..] {
            let pat = self.pattern(s.kid(arm, 0)?)?;
            writeln!(out, "{p}| {pat} =>").unwrap();
            let body = s.kid(arm, 1)?;
            if s.kind(body) == K::Block {
                self.block(out, body, ind + 2, tail)?;
            } else {
                self.expr_stmt(out, body, ind + 2, tail)?;
            }
        }
        Ok(())
    }

    /// A `match` value; arms one per line at `arm_ind`.
    pub(super) fn match_expr(&mut self, n: NodeId, arm_ind: usize) -> R<String> {
        let s = self.s;
        let kids = s.kids(n);
        let sc = self.scrutinee(kids[0])?;
        let mut out = format!("match {sc} with");
        for &arm in &kids[1..] {
            let pat = self.pattern(s.kid(arm, 0)?)?;
            let v = self.block_value(s.kid(arm, 1)?)?;
            if v.contains('←') {
                bail!("{}: an effect inside a `match` value", self.f.name);
            }
            write!(out, "\n{}| {pat} => {v}", pad(arm_ind)).unwrap();
        }
        Ok(out)
    }

    /// `while let P = e { body }`: a fuel-bounded `Hirsel.loop` over the one local it updates.
    pub(super) fn while_stmt(&mut self, out: &mut String, n: NodeId, ind: usize) -> R<()> {
        let s = self.s;
        let p = pad(ind);
        let kids = s.kids(n);
        let (cond, body) = (kids[0], kids[1]);
        let mut carried = BTreeSet::new();
        s.walk(body, &mut |m| {
            if s.kind(m) == K::Assign {
                let root = s.text(s.kids(m)[0]).split('.').next().unwrap().to_owned();
                carried.insert(root);
            }
        });
        let carried: Vec<String> = carried
            .into_iter()
            .filter(|c| self.muts.contains(c))
            .collect();
        let [v] = carried.as_slice() else {
            bail!(
                "{}: a loop must update exactly one outer local (it updates {carried:?})",
                self.f.name
            );
        };
        let v = mangle(v);
        writeln!(out, "{p}{v} ← Hirsel.loop {v} fun {v} => do").unwrap();
        writeln!(out, "{p}  let mut {v} := {v}").unwrap();
        let ret = |o: &mut String, i: usize, step: &str| {
            writeln!(o, "{}pure (Hirsel.Step.{step} {v})", pad(i)).unwrap()
        };
        if s.kind(cond) == K::ConditionLet {
            let pat = self.pattern(s.kid(cond, 0)?)?;
            let sc = self.scrutinee(s.kid(cond, 1)?)?;
            writeln!(out, "{p}  match {sc} with").unwrap();
            writeln!(out, "{p}  | {pat} =>").unwrap();
            self.loop_body(out, body, ind + 4)?;
            ret(out, ind + 4, "next");
            writeln!(out, "{p}  | _ =>").unwrap();
            ret(out, ind + 4, "done");
        } else {
            let c = self.cond(cond)?;
            writeln!(out, "{p}  if {c} then").unwrap();
            self.loop_body(out, body, ind + 4)?;
            ret(out, ind + 4, "next");
            writeln!(out, "{p}  else").unwrap();
            ret(out, ind + 4, "done");
        }
        Ok(())
    }

    pub(super) fn loop_body(&mut self, out: &mut String, body: NodeId, ind: usize) -> R<()> {
        let s = self.s;
        let mut bad = None;
        s.walk(body, &mut |m| {
            if matches!(s.kind(m), K::Break | K::Continue | K::Labelled) {
                bad = Some(s.text(m).to_owned());
            }
        });
        if let Some(b) = bad {
            bail!("{}: `{b}` inside a loop", self.f.name);
        }
        for &st in s.kids(body) {
            if s.kind(st) == K::ExpressionStatement && s.kind(s.kid(st, 0)?) == K::Return {
                let v = s.kid(s.kid(st, 0)?, 0)?;
                if self.result_ctor(v, "Error")?.is_none() {
                    bail!("{}: a loop body returns a value", self.f.name);
                }
            }
            self.stmt(out, st, ind, Tail::Unit, false)?;
        }
        Ok(())
    }

    /// `f(…, mut out) ?=> |p| e`, where `f` is a pure prelude function updating `out`.
    pub(super) fn map_error_stmt(&mut self, out: &mut String, n: NodeId, ind: usize) -> R<()> {
        let s = self.s;
        let p = pad(ind);
        let (call, arm) = (s.kid(n, 0)?, s.kid(n, 1)?);
        let Resolved::Prelude(pf) = self.resolve(call)? else {
            bail!("{}: `?=>` on `{}`", self.f.name, s.text(call));
        };
        let Some(m) = pf.mut_out else {
            bail!("{}: `?=>` on `{}`", self.f.name, pf.path)
        };
        let args: Vec<NodeId> = s.kids(call)[1..].to_vec();
        let target = args.get(m).copied().ok_or("missing mut argument")?;
        if s.kind(target) != K::Unary || s.tok(target) != "mut" {
            bail!("{}: argument {m} of {} must be `mut`", self.f.name, pf.path);
        }
        let local = s.text(s.kid(target, 0)?);
        if !self.muts.contains(local) {
            bail!("{}: `mut {local}` is not a `let mut` local", self.f.name);
        }
        let a = self.arg_list(&args)?;
        let v = mangle(local);
        let tmp = format!("{local}'");
        let ap = self.pattern(s.kid(arm, 0)?)?;
        let ae = self.expr(s.kid(arm, 1)?)?;
        writeln!(out, "{p}let {tmp} := {}{a}", pf.lean).unwrap();
        writeln!(out, "{p}{v} := {tmp}.2").unwrap();
        writeln!(out, "{p}Hirsel.tryMap {tmp}.1 (fun {ap} => {ae})").unwrap();
        Ok(())
    }
}

/// `a.f.g = v` as a record update of `a`.
pub(super) fn update(segs: &[&str], rhs: &str) -> String {
    if segs.len() == 1 {
        return rhs.to_owned();
    }
    let base = segs.iter().map(|x| mangle(x)).collect::<Vec<_>>();
    let mut v = rhs.to_owned();
    for i in (1..segs.len()).rev() {
        let path = base[..i].join(".");
        v = format!("{{ {path} with {} := {v} }}", base[i]);
    }
    v
}
