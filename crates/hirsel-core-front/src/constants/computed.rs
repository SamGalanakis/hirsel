//! Computed constants (LANG-20): an initialiser that calls a function or names a
//! computed constant. Which kind a constant is follows from its initialiser and the
//! constants it names, without running anything. A computed constant's initialiser is
//! declared as a generated function with no parameter, under the constant's own path
//! (PROG-10), which checking lowers like any body; the driver runs its retained code.
use super::*;

impl Program<'_> {
    /// This request's computed constants, each with the site its refusal names:
    /// the constant's declaration.
    pub fn computed_constants(&self) -> Vec<(DefPath, Site)> {
        (self.constants.base_len()..self.constants.len())
            .filter(|&i| self.constants[i].computed && self.constants[i].ty != ERROR)
            .map(|i| {
                let c = &self.constants[i];
                (c.path.clone(), self.site(c.at, self.item(c.at).root))
            })
            .collect()
    }

    /// Whether constant `i`'s initialiser calls a function or names a computed
    /// constant. Constructions of records and variants are manifest.
    pub(super) fn calls_in_initialiser(&self, i: usize) -> bool {
        let c = &self.constants[i];
        let item = self.item(c.at);
        let names = |n: NodeId| self.resolved(c.at, n).map(|u| u.target);
        let mut stack = vec![c.expr];
        while let Some(n) = stack.pop() {
            let node = item.tree.node(n);
            let kids = item.tree.children(n);
            match node.kind {
                NodeKind::Call => {
                    let head = kids[0];
                    let constructs = match names(head) {
                        Some(Target::Definition(id)) => {
                            matches!(self.by_definition[id.0 as usize], Some(Def::Type(_)))
                        }
                        Some(Target::ContextualConstructor { .. }) => true,
                        // An unresolved head, a method or a builtin is a call.
                        _ => false,
                    };
                    if !constructs {
                        return true;
                    }
                }
                NodeKind::Path => match names(n) {
                    Some(Target::Definition(id))
                        if matches!(self.by_definition[id.0 as usize], Some(Def::Fn(_))) =>
                    {
                        return true;
                    }
                    Some(Target::Definition(id))
                        if self.constant_ids[id.0 as usize]
                            .is_some_and(|d| self.constants[d as usize].computed) =>
                    {
                        return true;
                    }
                    _ => {}
                },
                _ => {}
            }
            stack.extend_from_slice(kids);
        }
        false
    }

    /// Declare constant `i` computed: refuse a type that holds a function, and
    /// declare its initialiser as a function under the constant's path. A String
    /// constant is never computed: the static-string rule refuses its call.
    pub(super) fn declare_computed(&mut self, i: usize) {
        let c = &self.constants[i];
        let (at, ty, expr) = (c.at, c.ty, c.expr);
        let item = self.item(at);
        let kids = item.tree.children(item.root);
        let type_node = kids[kids.len() - 2];
        if self.holds_function(ty) {
            self.const_refuse(
                at,
                type_node,
                "a computed constant's type holds no function value",
            );
            self.constants[i].ty = ERROR;
            return;
        }
        self.constants[i].computed = true;
        let path = self.constants[i].path.clone();
        let site = self.site(at, item.root);
        let id = self.fns.len() as u32;
        // `fn() -> T`: an initialiser is not `waits` (LANG-20).
        let callable =
            self.types
                .intern(TyKind::Fn(vec![], ty, false, crate::types::Callable::Plain));
        self.fns.push(FnDecl {
            is_test: false,
            cold: false,
            path,
            site,
            at,
            generics: vec![],
            params: vec![],
            has_region_parameters: false,
            result: ty,
            callable,
            result_site: self.site(at, type_node),
            anchors: vec![],
            region_anchors: vec![],
            outlives: vec![],
            waits: false,
            body: Some(expr),
            drop_of: None,
            clone_of: None,
        });
        self.add_path(Def::Fn(id));
    }

    /// A `fn` type or `StaticFn` anywhere in `ty`'s value.
    fn holds_function(&self, ty: TyId) -> bool {
        let mut stack = vec![ty];
        let mut seen = std::collections::HashSet::new();
        while let Some(ty) = stack.pop() {
            if !seen.insert(ty) {
                continue;
            }
            match self.types.kind(ty) {
                TyKind::Fn(..) => return true,
                TyKind::Tuple(xs) => stack.extend(xs.iter().copied()),
                TyKind::Array(x, _) => stack.push(*x),
                TyKind::Adt(d, args) => {
                    let decl = &self.decls[d.index()];
                    if decl.role == Some(Role::StaticFn) {
                        return true;
                    }
                    let args = args.clone();
                    let fields: Vec<TyId> = decl.all_fields().map(|f| f.ty).collect();
                    stack.extend(args.iter().copied());
                    for field in fields {
                        if !matches!(self.types.kind(field), TyKind::Param(_)) {
                            stack.push(field);
                        }
                    }
                }
                _ => {}
            }
        }
        false
    }
}
