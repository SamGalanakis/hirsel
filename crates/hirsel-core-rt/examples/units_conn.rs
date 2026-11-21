//! Round trips over the unit connection path on the untied pool.
//! Usage: HIRSEL_UNTIED=<workers> units_conn <ready|parked> <warm> <n> <size>
// The plain system allocator, as under the runtime archive's standalone
// allocator; the guard's checking allocator only in debug builds.
#[cfg(debug_assertions)]
hirsel_guard::install!();
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let num = |i: usize| a[i].parse::<u64>().expect("a number");
    let parked = match a[1].as_str() {
        "ready" => false,
        "parked" => true,
        m => panic!("mode {m}: ready or parked"),
    };
    std::process::exit(hirsel_core_rt::units_conn_pingpong(parked, num(2), num(3), num(4)).into());
}
