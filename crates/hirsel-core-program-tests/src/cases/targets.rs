//! Runs on each build target's retained code (PROG-6, PROG-10): every `run` line also runs
//! on the targets other than the default one, and `// expect: on <triple> run f(args) = R` states
//! what one target computes. `// expect: on <triple> refuses <line>` states that the build
//! for that target is refused with exactly that line (PROG-6 `target.unsupported`); its runs
//! are not made there.
use hirsel_core_driver::CheckProduct;
use hirsel_core_ir::{Anchor, FuncId, Identity, ItemId, Module};
use hirsel_core_layout::Target;
use std::cell::RefCell;
use std::collections::HashMap;

type Built = Result<(Module, HashMap<String, FuncId>), String>;

pub(super) struct OnTargets<'a> {
    product: &'a CheckProduct,
    roots: &'a [Identity],
    built: RefCell<HashMap<Target, std::rc::Rc<Built>>>,
    refused: Vec<(Target, String)>,
}

impl<'a> OnTargets<'a> {
    pub(super) fn new(product: &'a CheckProduct, roots: &'a [Identity], source: &str) -> Self {
        let refused = source
            .lines()
            .filter_map(|l| l.trim().strip_prefix("// expect: on ")?.split_once(' '))
            .filter_map(|(triple, rest)| {
                let target = Target::from_triple(triple).expect("a refusal names a target");
                Some((target, rest.strip_prefix("refuses ")?.to_owned()))
            })
            .collect();
        Self {
            product,
            roots,
            built: RefCell::default(),
            refused,
        }
    }

    /// Whether the build for `target` is expected to be refused.
    pub(super) fn refuses(&self, target: Target) -> bool {
        self.refused.iter().any(|(t, _)| *t == target)
    }

    /// Each expected refusal that its target's build does not make, exactly.
    pub(super) fn refusals(&self) -> Vec<String> {
        self.refused
            .iter()
            .filter_map(|(target, want)| {
                let got = match &*self.built(*target) {
                    Ok(_) => "a build".to_owned(),
                    Err(e) if e == want => return None,
                    Err(e) => e.clone(),
                };
                Some(format!("on {}: want `{want}`, got {got}", target.triple()))
            })
            .collect()
    }

    fn built(&self, target: Target) -> std::rc::Rc<Built> {
        self.built
            .borrow_mut()
            .entry(target)
            .or_insert_with(|| self.build(target).into())
            .clone()
    }

    /// `call` on every target but the default one, whose run the caller makes; a site is
    /// rendered by that run's `site`, through the item's path in its `host` module.
    pub(super) fn others(
        &self,
        call: &str,
        want: &[&str],
        host: &Module,
        site: &dyn Fn(Anchor) -> String,
    ) -> Vec<String> {
        Target::ALL
            .into_iter()
            .filter(|t| *t != hirsel_core_driver::default_target() && !self.refuses(*t))
            .filter_map(|t| self.run_with(t.triple(), call, want, Some((host, site))))
            .collect()
    }

    pub(super) fn run(&self, triple: &str, call: &str, want: &[&str]) -> Option<String> {
        self.run_with(triple, call, want, None)
    }

    fn run_with(
        &self,
        triple: &str,
        call: &str,
        want: &[&str],
        host: Option<(&Module, &dyn Fn(Anchor) -> String)>,
    ) -> Option<String> {
        let Some(target) = Target::from_triple(triple) else {
            return Some(format!("on {triple}: no such target"));
        };
        let built = self.built(target);
        let (module, functions) = match &*built {
            Ok(built) => built,
            Err(e) => return Some(format!("on {triple}: {e}")),
        };
        let site = |a: Anchor| {
            let path = &module.items[a.item.index()];
            let Some((host, site)) = host else {
                return format!("{path:?}@{}..{}", a.start, a.end);
            };
            match host.items.iter().position(|p| p == path) {
                Some(i) => site(Anchor {
                    item: ItemId(i as u32),
                    ..a
                }),
                None => format!("{path:?}"),
            }
        };
        super::run_expect(module, functions, call, want, &site).map(|f| format!("on {triple}: {f}"))
    }

    fn build(&self, target: Target) -> Built {
        let units = self
            .product
            .units_for(self.roots.iter().cloned(), 65_536, target)
            .map_err(|e| e.to_string())?;
        let unit_modules = units.modules().unwrap();
        let module = match self.roots.first() {
            Some(entry) => {
                let (module, ..) = hirsel_core_materialize::join(&unit_modules, entry)
                    .map_err(|e| format!("join: {e}"))?;
                module
            }
            None => Module::default(),
        };
        let functions = module
            .functions
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                let key = &module.instances[f.instance.index()];
                (key.definition.package_is("app", "1")
                    && key.arguments.is_empty()
                    && f.body.is_some())
                .then(|| (key.definition.0.name.clone(), FuncId(i as u32)))
            })
            .collect();
        Ok((module, functions))
    }
}
