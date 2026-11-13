#[cfg(debug_assertions)]
hirsel_guard::install!(mimalloc::MiMalloc: mimalloc::MiMalloc);
#[cfg(not(debug_assertions))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;
unsafe extern "C" {
    #[link_name = "mi_malloc"]
    fn native_allocate(bytes: usize) -> *mut u8;
    #[link_name = "mi_free"]
    fn native_free(pointer: *mut u8);
}
mod cost;
fn main() {
    cost::main();
}
