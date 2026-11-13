//! Measurement-only emitter. Cranelift's own profiler supplies nested pass
//! boundaries; hardware counters and resettable RSS high-water marks are sampled
//! at every boundary. Observer bookkeeping is outside the instruction intervals.
hirsel_guard::install!();
#[path = "../tests/support/front.rs"]
mod front;

use cranelift_codegen::timing::{self, Pass, Profiler};
use cranelift_module::{Module as _, ModuleReloc};
use hirsel_cranelift::core::{self, Options, profile::Observer};
use hirsel_intern::instructions::InstructionCounter;
use std::{
    any::Any,
    cell::RefCell,
    collections::BTreeMap,
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::Path,
    rc::Rc,
};

#[derive(Default)]
struct Row {
    instructions: u64,
    peak_kib: u64,
    segments: u64,
}
struct Meter {
    counter: InstructionCounter,
    clear: File,
    last: u64,
    stack: Vec<&'static str>,
    rows: BTreeMap<&'static str, Row>,
}
fn rss() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let get = |key| {
        status
            .lines()
            .find_map(|s| s.strip_prefix(key))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    (get("VmRSS:"), get("VmHWM:"))
}
impl Meter {
    fn new() -> Self {
        let counter = InstructionCounter::open().unwrap();
        let last = counter.read().unwrap();
        Self {
            counter,
            clear: File::options()
                .write(true)
                .open("/proc/self/clear_refs")
                .unwrap(),
            last,
            stack: vec![],
            rows: BTreeMap::new(),
        }
    }
    fn boundary(&mut self, enter: Option<&'static str>) {
        let now = self.counter.read().unwrap();
        let (_, peak) = rss();
        if let Some(name) = self.stack.last() {
            let row = self.rows.entry(name).or_default();
            row.instructions += now - self.last;
            row.peak_kib = row.peak_kib.max(peak);
            row.segments += 1;
        }
        if let Some(name) = enter {
            if matches!(name, "intake" | "build_clif" | "codegen") {
                eprintln!("BEGIN\t{name}\t{now}\t{}", rss().0);
            }
            self.stack.push(name);
        } else {
            let name = self.stack.pop().unwrap();
            let row = &self.rows[name];
            eprintln!(
                "END\t{name}\t{}\t{}\t{}",
                row.instructions, row.peak_kib, row.segments
            );
        }
        self.clear.seek(SeekFrom::Start(0)).unwrap();
        self.clear.write_all(b"5").unwrap();
        self.last = self.counter.read().unwrap();
    }
}
#[derive(Clone)]
struct Measure(Rc<RefCell<Meter>>, bool);
struct Span(Measure);
impl Drop for Span {
    fn drop(&mut self) {
        self.0.0.borrow_mut().boundary(None);
    }
}
impl Measure {
    fn scope(&self, name: &'static str) -> Span {
        let token = Span(self.clone());
        self.0.borrow_mut().boundary(Some(name));
        token
    }
}
impl Profiler for Measure {
    fn start_pass(&self, pass: Pass) -> Box<dyn Any> {
        // Cranelift's pass token is boxed by its profiling API in both the
        // default and this profiler. No lowering instruction allocates a token.
        Box::new(self.scope(pass.description()))
    }
}
impl Observer for Measure {
    type Span = Span;
    fn enter(&self, name: &'static str) -> Span {
        self.scope(name)
    }
    fn define(
        &self,
        object: &mut cranelift_object::ObjectModule,
        function: cranelift_module::FuncId,
        ctx: &mut cranelift_codegen::Context,
    ) -> cranelift_module::ModuleResult<()> {
        {
            let _span = self.scope("profile_counts");
            eprintln!(
                "CLIF\t{}\t{}\t{}\t{}\t{}\t{}",
                function.as_u32(),
                ctx.func.dfg.num_insts(),
                ctx.func.dfg.num_blocks(),
                ctx.func.sized_stack_slots.len(),
                ctx.func.dfg.ext_funcs.len(),
                ctx.func.dfg.signatures.len()
            );
        }
        if self.1 {
            return Ok(());
        }
        {
            let _span = self.scope("codegen");
            ctx.compile(object.isa(), &mut Default::default())?;
        }
        let _span = self.scope("object_function");
        let code = ctx.compiled_code().unwrap();
        let relocs: Vec<_> = code
            .buffer
            .relocs()
            .iter()
            .map(|r| ModuleReloc::from_mach_reloc(r, &ctx.func, function))
            .collect();
        object.define_function_bytes(
            function,
            code.buffer.alignment as u64,
            code.buffer.data(),
            &relocs,
        )
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    let measure = Measure(
        Rc::new(RefCell::new(Meter::new())),
        args.iter().any(|a| a == "--ir-counts-only"),
    );
    let _previous = timing::set_thread_profiler(Box::new(measure.clone()));
    let total = measure.scope("total");
    let module = {
        let _span = measure.scope("intake");
        let text = std::fs::read_to_string(&args[1])?;
        if let Some(i) = args.iter().position(|a| a == "--front") {
            front::lower(Path::new(&args[i + 1]), &text).0
        } else {
            hirsel_core_ir::text::parse(&text).map_err(|e| format!("{e:?}"))?
        }
    };
    let instructions = module
        .functions
        .iter()
        .filter_map(|f| f.body.as_ref())
        .map(|b| b.insts.len())
        .sum::<usize>();
    let terms = module
        .functions
        .iter()
        .filter_map(|f| f.body.as_ref())
        .map(|b| b.terminators.len())
        .sum::<usize>();
    println!("input\t{instructions}\t{terms}\t{}", module.functions.len());
    let layouts = {
        let _span = measure.scope("layout_intake");
        hirsel_core_layout::compute(&module.types).map_err(|e| format!("{e:?}"))?
    };
    let object = core::emit_object_profiled(
        &module,
        &layouts,
        &Options {
            target: "x86_64-unknown-linux-gnu",
            optimize: args.iter().any(|a| a == "--speed"),
        },
        &measure,
    )?;
    {
        let _span = measure.scope("object_file");
        std::fs::write(&args[2], object.bytes)?;
    }
    {
        let _span = measure.scope("intake_drop");
        drop(module);
        drop(layouts);
    }
    drop(total);
    println!("stage\tinstructions_exclusive\tpeak_rss_kib\tsegments");
    for (name, row) in &measure.0.borrow().rows {
        println!(
            "{name}\t{}\t{}\t{}",
            row.instructions, row.peak_kib, row.segments
        );
    }
    Ok(())
}
