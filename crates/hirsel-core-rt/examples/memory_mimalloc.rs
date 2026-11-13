#[cfg(debug_assertions)]
hirsel_guard::install!(mimalloc::MiMalloc: mimalloc::MiMalloc);
#[cfg(not(debug_assertions))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;
mod cost;
fn main() {
    cost::main();
}
