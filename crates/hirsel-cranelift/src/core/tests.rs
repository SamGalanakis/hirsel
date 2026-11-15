use super::*;
use std::cell::RefCell;

#[derive(Default)]
struct Code(RefCell<Vec<u8>>);

impl profile::Observer for Code {
    type Span = ();
    fn enter(&self, _: &'static str) {}
    fn define(
        &self,
        object: &mut ObjectModule,
        function: cranelift_module::FuncId,
        context: &mut cranelift_codegen::Context,
    ) -> cranelift_module::ModuleResult<()> {
        object.define_function(function, context)?;
        if function.as_u32() == 0 {
            *self.0.borrow_mut() = context.compiled_code().unwrap().code_buffer().to_vec();
        }
        Ok(())
    }
}

#[test]
fn fixed_append_copy_does_not_spill_the_index_through_a_libc_call() {
    struct Calls(RefCell<usize>);
    impl profile::Observer for Calls {
        type Span = ();
        fn enter(&self, _: &'static str) {}
        fn define(
            &self,
            object: &mut ObjectModule,
            function: cranelift_module::FuncId,
            context: &mut cranelift_codegen::Context,
        ) -> cranelift_module::ModuleResult<()> {
            object.define_function(function, context)?;
            if function.as_u32() == 0 {
                *self.0.borrow_mut() = context
                    .func
                    .layout
                    .blocks()
                    .flat_map(|block| context.func.layout.block_insts(block))
                    .filter(|&inst| context.func.dfg.insts[inst].opcode().is_call())
                    .count();
            }
            Ok(())
        }
    }
    let input = text::parse(
        "core v1\nsig g0 = (ptr, ptr, u64) -> (u64)\n\
        inst n0 = test@0/append/copy [] []\nfn f0 = n0 g0 {\n\
        addr a0 = v0 index v2 : u8\naddr a1 = v1 : u8\nb0(v0: ptr, v1: ptr, v2: u64):\n\
        v3: u64 = const 88\naddress a0\naddress a1\ncopy a0, a1, v3\n\
        v4: u64 = const 1\nv5: u64 = wrapping_add v2, v4\nreturn v5\n}\n",
    )
    .unwrap();
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        for optimize in [false, true] {
            let calls = Calls(RefCell::new(0));
            emit_object_profiled(
                &input,
                &[],
                &Options {
                    target,
                    optimize,
                    debug_lines: false,
                },
                hirsel_core_ids::ContentHash([0; 32]),
                &calls,
            )
            .unwrap();
            assert_eq!(calls.0.into_inner(), 0, "{target}, optimize={optimize}");
        }
    }
}

#[test]
fn typed_sequence_projections_emit_only_the_memory_offset() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        for optimize in [false, true] {
            for (word, offset) in [("len", 8), ("cap", 16)] {
                let emit = |projection: &str| {
                    let input = text::parse(&format!(
                        "core v1\ntype t0 = own \"List\" {{ items: seq u64 }}\n\
                         sig g0 = (ptr) -> (u64)\ninst n0 = test@0/offset/read [] []\n\
                         fn f0 = n0 g0 {{\n{projection}\nb0(v0: ptr):\n\
                         v1: u64 = const {offset}\naddress a0\naddress a1\n\
                         v2: u64 = read a1\nreturn v2\n}}\n"
                    ))
                    .unwrap();
                    let layouts = hirsel_core_layout::compute(&input.types).unwrap();
                    let code = Code::default();
                    emit_object_profiled(
                        &input,
                        &layouts,
                        &Options {
                            target,
                            optimize,
                            debug_lines: false,
                        },
                        hirsel_core_ids::ContentHash([0; 32]),
                        &code,
                    )
                    .unwrap();
                    code.0.into_inner()
                };
                let plain = emit("addr a0 = v0 : u64\naddr a1 = a0 byte v1 : u64");
                let typed = emit(&format!(
                    "addr a0 = v0 : t0\naddr a1 = a0 field 0 seq {word} : u64"
                ));
                assert!(!plain.is_empty());
                assert_eq!(typed, plain, "{target}, optimize={optimize}, seq {word}");
            }
        }
    }
}
