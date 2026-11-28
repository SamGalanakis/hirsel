use super::*;
use crate::type_limits::{
    Expansion, Extent, LAYOUT_BYTES, TYPE_DEPTH, TYPE_NODES, TooDeep, structural_extent,
};

pub(super) struct TypeLimits<'a> {
    inline: Expansion<'a>,
    structural: HashMap<TyId, Extent>,
    admitted: HashSet<TyId>,
}

impl<'a> TypeLimits<'a> {
    pub(super) fn new(p: &'a Program<'_>) -> Self {
        Self {
            inline: Expansion::for_body(&p.decls),
            structural: HashMap::default(),
            admitted: HashSet::default(),
        }
    }
}

impl Cx<'_, '_, '_> {
    /// How a value of `ty` is carried in this body.
    pub(super) fn rep(&mut self, ty: TyId) -> Result<Rep, &'static str> {
        if let Some(scalar) = scalar_kind(ty) {
            return Ok(Rep::Scalar(Ty::Scalar(scalar)));
        }
        if let Some(&rep) = self.reps.get(&ty) {
            return rep;
        }
        if !self.admit_type(ty, self.node) {
            return Err("the type exceeds the checker's resource limits");
        }
        self.e.native_range = None;
        let rep = self.e.rep(self.p, &mut self.t, ty);
        if let Some(size) = self.e.native_range.take() {
            self.refuse_type_layout(self.node, size);
            return Err("the type exceeds the checker's resource limits");
        }
        if let Ok(rep) = rep
            && !self.admit_type_layout(ty, rep)
        {
            return Err("the type exceeds the checker's resource limits");
        }
        self.reps.insert(ty, rep);
        rep
    }

    pub(super) fn refuse_type_resource(&mut self, n: NodeId, message: String) {
        self.out
            .resource_refusal
            .get_or_insert_with(|| crate::ResourceRefusal {
                site: self.p.site(self.at, n),
                message,
            });
        self.halt();
    }

    /// Admit a type before a query walks its expanded tree or renders its name.
    pub(super) fn admit_type(&mut self, ty: TyId, n: NodeId) -> bool {
        if ty <= HOLE || self.type_limits.admitted.contains(&ty) {
            return true;
        }
        if self.out.resource_refusal.is_some() {
            return false;
        }
        let structural = structural_extent(&self.t, &mut self.type_limits.structural, ty, 1);
        let extent = structural.and_then(|shape| {
            if shape.nodes > TYPE_NODES || shape.depth > TYPE_DEPTH {
                return Ok(shape);
            }
            let inline = self.type_limits.inline.extent(&mut self.t, ty)?;
            Ok(Extent {
                nodes: shape.nodes.max(inline.nodes),
                depth: shape.depth.max(inline.depth),
            })
        });
        let message = match extent {
            Ok(e) if e.nodes > TYPE_NODES => format!(
                "the body type expands to at least {} type nodes, past the checker's limit of \
                 {TYPE_NODES} nodes in one type",
                e.nodes
            ),
            Ok(e) if e.depth > TYPE_DEPTH => format!(
                "the body type nests types {} deep, past the checker's limit of \
                 {TYPE_DEPTH} levels in one type",
                e.depth
            ),
            Err(TooDeep) => format!(
                "the body type nests types more than {TYPE_DEPTH} deep, \
                 the checker's limit of levels in one type"
            ),
            Ok(_) => {
                self.type_limits.admitted.insert(ty);
                return true;
            }
        };
        self.refuse_type_resource(n, message);
        false
    }

    pub(super) fn admit_type_layout(&mut self, ty: TyId, rep: Rep) -> bool {
        if let Rep::Place(id) = rep
            && !has_param(&self.t, ty)
            && let size = self.e.size(id)
            && size > LAYOUT_BYTES
        {
            self.refuse_type_layout(self.node, size.into());
            return false;
        }
        true
    }
    fn refuse_type_layout(&mut self, n: NodeId, size: u128) {
        self.refuse_type_resource(
            n,
            format!(
                "the body type is {size} bytes, past the checker's limit of \
                 {LAYOUT_BYTES} bytes for one type (FAIL-17)"
            ),
        );
    }

    pub(super) fn check_type(&mut self, ty: TyId, n: NodeId) -> TyId {
        if self.admit_type(ty, n) { ty } else { ERROR }
    }
}
