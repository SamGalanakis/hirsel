//! Diagnostic hooks for exclusive backend measurements, absent from ordinary
//! emission after monomorphization. No instructions or products are validated.
use cranelift_codegen::Context;
use cranelift_module::{FuncId, Module, ModuleResult};
use cranelift_object::ObjectModule;

pub trait Observer {
    type Span;
    fn enter(&self, stage: &'static str) -> Self::Span;
    fn define(
        &self,
        object: &mut ObjectModule,
        function: FuncId,
        context: &mut Context,
    ) -> ModuleResult<()> {
        object.define_function(function, context)
    }
}
impl Observer for () {
    type Span = ();
    #[inline(always)]
    fn enter(&self, _: &'static str) {}
}
