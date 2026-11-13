//! System V AMD64 result classification, adapted from rustc_target's
//! callconv/x86_64.rs: integer class wins within each eightbyte; aggregates
//! over two eightbytes use sret. Only CoreIR's naturally aligned scalars occur.
use super::*;

pub(super) struct Return {
    pub ty: String,
    pub storage: String,
    pub indirect: bool,
    pub fields: Vec<Ty>,
    pub offsets: Vec<u64>,
    pub regs: Vec<String>,
}
pub(super) fn extension(t: Ty) -> &'static str {
    if size(t) >= 4 || t.is_float() || t == Ty::Ptr {
        ""
    } else if t.int().is_some_and(|x| x.0) {
        " signext"
    } else {
        " zeroext"
    }
}
impl Return {
    pub fn internal(fields: &[Ty]) -> Self {
        let mut fields = fields.to_vec();
        fields.push(Ty::Scalar(Scalar::U8));
        Self::new(&fields)
    }

    pub fn parameters(&self, sig: &Signature) -> Vec<String> {
        let mut params = vec![];
        if self.indirect {
            params.push(format!("ptr sret({}) %sret", self.storage));
        }
        params.push("ptr %context".into());
        if sig.dest.is_some() {
            params.push("ptr %dest".into());
        }
        for (i, p) in sig.params.iter().enumerate() {
            let t = match p {
                Param::Value(t) => format!("{}{}", ty(*t), extension(*t)),
                Param::Address(_) => "ptr".into(),
            };
            params.push(format!("{t} %p{i}"));
        }
        params
    }

    pub fn return_type(&self) -> String {
        match self.fields.as_slice() {
            [t] if !extension(*t).is_empty() => format!("{} {}", extension(*t).trim(), self.ty),
            _ => self.ty.clone(),
        }
    }

    pub fn new(fields: &[Ty]) -> Self {
        let mut offsets = vec![];
        let mut end = 0u64;
        let mut alignment = 1;
        for &t in fields {
            let n = size(t);
            alignment = alignment.max(n);
            end = end.next_multiple_of(n);
            offsets.push(end);
            end += n;
        }
        let extent = end.next_multiple_of(alignment);
        let storage = format!("[{} x i8]", extent.max(16));
        let indirect = fields.len() > 1 && extent > 16;
        let regs = if fields.len() <= 1 {
            fields.iter().map(|t| ty(*t)).collect()
        } else if indirect {
            vec![]
        } else {
            (0..end.div_ceil(8))
                .map(|chunk| {
                    let fs = fields
                        .iter()
                        .zip(&offsets)
                        .filter(|(_, off)| **off / 8 == chunk)
                        .collect::<Vec<_>>();
                    if fs.iter().any(|(t, _)| !t.is_float()) {
                        format!("i{}", (end - chunk * 8).min(8) * 8)
                    } else if fs.iter().any(|(t, _)| size(**t) == 8) {
                        "double".into()
                    } else if fs.len() == 2 {
                        "<2 x float>".into()
                    } else {
                        "float".into()
                    }
                })
                .collect::<Vec<_>>()
        };
        let ty = match regs.as_slice() {
            [] => "void".into(),
            [one] => one.clone(),
            _ => format!("{{ {} }}", regs.join(", ")),
        };
        Self {
            ty,
            storage,
            indirect,
            fields: fields.to_vec(),
            offsets,
            regs,
        }
    }
}

impl FunctionEmitter<'_> {
    fn offset(&mut self, p: &str, n: u64) -> String {
        self.calc(format!("getelementptr i8, ptr {p}, i64 {n}"))
    }
    pub(super) fn return_values(&mut self, values: &[String]) {
        let mut returned = values.to_vec();
        if self.has_status {
            returned.push("0".into());
        }
        self.return_abi_values(&returned);
    }
    pub(super) fn return_failure(&mut self) {
        let mut values = vec!["zeroinitializer".into(); self.abi.fields.len()];
        *values.last_mut().unwrap() = "1".into();
        self.return_abi_values(&values);
    }
    fn return_abi_values(&mut self, values: &[String]) {
        if self.abi.fields.len() <= 1 {
            if let Some(x) = values.first() {
                self.line(format!("ret {} {x}", self.abi.ty));
            } else {
                self.line("ret void");
            }
            return;
        }
        let p = if self.abi.indirect {
            "%sret".into()
        } else {
            self.temp(&self.abi.storage.clone())
        };
        // Padding is defined, so register coercions never observe poison.
        self.line(format!(
            "store {} zeroinitializer, ptr {p}, align 8",
            self.abi.storage
        ));
        for ((t, off), x) in self
            .abi
            .fields
            .clone()
            .iter()
            .zip(self.abi.offsets.clone())
            .zip(values)
        {
            let at = self.offset(&p, off);
            self.line(format!("store {} {x}, ptr {at}, align 1", ty(*t)));
        }
        if self.abi.indirect {
            self.line("ret void");
            return;
        }
        let mut r = "poison".to_string();
        for (i, t) in self.abi.regs.clone().iter().enumerate() {
            let at = self.offset(&p, i as u64 * 8);
            let x = self.calc(format!("load {t}, ptr {at}, align 1"));
            r = if self.abi.regs.len() == 1 {
                x
            } else {
                self.calc(format!("insertvalue {} {r}, {t} {x}, {i}", self.abi.ty))
            };
        }
        self.line(format!("ret {} {r}", self.abi.ty));
    }
    pub(super) fn call(&mut self, site: &CallSite) -> Result<()> {
        if site.callee == Callee::Runtime(RtOp::DestId) {
            self.intrinsics
                .insert("declare i64 @hirsel_core_dest_id()".into());
            let value = self.calc("call i64 @hirsel_core_dest_id()");
            let result = site.results.get(&self.body.pools.values).unwrap()[0];
            self.put(result, &value);
            return Ok(());
        }
        if site.callee == Callee::Runtime(RtOp::CompareBytes) {
            let operands = site.args.get(&self.body.pools.args).unwrap();
            let args = operands
                .iter()
                .zip(["ptr", "i64", "ptr", "i64"])
                .map(|(arg, t)| {
                    let Arg::Value(v) = *arg else { unreachable!() };
                    (t.into(), self.get(v))
                })
                .collect::<Vec<_>>();
            let value = self.compare_bytes(&args);
            let result = site.results.get(&self.body.pools.values).unwrap()[0];
            self.put(result, &value);
            return Ok(());
        }
        let sig = &self.module.signatures[site.signature.index()];
        let has_status = match site.callee {
            Callee::Direct(f) => self.failures.may_fail[f.index()],
            _ => true,
        };
        let abi = if has_status {
            Return::internal(&sig.results)
        } else {
            Return::new(&sig.results)
        };
        let mut args = vec![];
        args.push("ptr %context".into());
        for arg in site.args.get(&self.body.pools.args).unwrap() {
            args.push(match *arg {
                Arg::Value(v) => {
                    let x = self.get(v);
                    format!("{}{} {x}", ty(self.vty(v)), extension(self.vty(v)))
                }
                Arg::Address(a) => format!("ptr {}", self.address(a)),
            });
        }
        let callee = match site.callee {
            Callee::Direct(f) => format!(
                "@{}{}",
                symbol(self.module, f),
                if has_status { "" } else { ".infallible" }
            ),
            Callee::Indirect(v) => self.get(v),
            Callee::Runtime(_) => return Err(Error::Unsupported("task runtime calls".into())),
        };
        let mut values = self.invoke(&abi, &callee, args);
        if has_status {
            let status = values.pop().unwrap();
            let failed = self.calc(format!("icmp ne i8 {status}, 0"));
            self.branch_failure(&failed, site.fail, None);
        }
        for (v, x) in site
            .results
            .get(&self.body.pools.values)
            .unwrap()
            .iter()
            .zip(values)
        {
            self.put(*v, &x);
        }
        Ok(())
    }
    fn invoke(&mut self, abi: &Return, callee: &str, mut args: Vec<String>) -> Vec<String> {
        let scratch = if abi.fields.len() > 1 {
            Some(self.temp(&abi.storage))
        } else {
            None
        };
        if abi.indirect {
            args.insert(
                0,
                format!("ptr sret({}) {}", abi.storage, scratch.as_ref().unwrap()),
            );
        }
        let call = format!("call {} {callee}({})", abi.return_type(), args.join(", "));
        let r = if abi.ty == "void" {
            self.line(call);
            None
        } else {
            Some(self.calc(call))
        };
        let mut values = vec![];
        if abi.fields.len() == 1 {
            values.push(r.unwrap());
        } else if let Some(p) = scratch {
            if !abi.indirect {
                for (i, t) in abi.regs.iter().enumerate() {
                    let x = if abi.regs.len() == 1 {
                        r.as_ref().unwrap().clone()
                    } else {
                        self.calc(format!(
                            "extractvalue {} {}, {i}",
                            abi.ty,
                            r.as_ref().unwrap()
                        ))
                    };
                    let at = self.offset(&p, i as u64 * 8);
                    self.line(format!("store {t} {x}, ptr {at}, align 1"));
                }
            }
            for (t, off) in abi.fields.iter().zip(&abi.offsets) {
                let at = self.offset(&p, *off);
                values.push(self.calc(format!("load {}, ptr {at}, align 1", ty(*t))));
            }
        }
        values
    }

    pub(super) fn infallible_wrapper(&mut self, name: &str, sig: &Signature, text: &mut String) {
        self.code.clear();
        self.allocations.clear();
        self.next = 0;
        self.has_status = true;
        self.abi = Return::internal(&sig.results);
        let mut args = vec!["ptr %context".into()];
        if sig.dest.is_some() {
            args.push("ptr %dest".into());
        }
        for (i, p) in sig.params.iter().enumerate() {
            args.push(match p {
                Param::Value(t) => format!("{}{} %p{i}", ty(*t), extension(*t)),
                Param::Address(_) => format!("ptr %p{i}"),
            });
        }
        let values = self.invoke(
            &Return::new(&sig.results),
            &format!("@{name}.infallible"),
            args,
        );
        self.return_values(&values);
        writeln!(
            text,
            "define {} @{name}({}) {{\nentry:\n{}{} }}",
            self.abi.return_type(),
            self.abi.parameters(sig).join(", "),
            self.allocations,
            self.code
        )
        .unwrap();
    }
}
