#[cfg(debug_assertions)]
hirsel_guard::install!();
mod cost;
fn main() {
    cost::main();
}
