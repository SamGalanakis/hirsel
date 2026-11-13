#[cfg(debug_assertions)]
hirsel_guard::install!();
unsafe extern "C" {
    #[link_name = "malloc"]
    fn native_allocate(bytes: usize) -> *mut u8;
    #[link_name = "free"]
    fn native_free(pointer: *mut u8);
}
mod cost;
fn main() {
    cost::main();
}
