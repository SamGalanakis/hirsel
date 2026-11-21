//! Round trips between a root task and a helper unit through the language's
//! entry points. Usage: [HIRSEL_UNTIED=<workers>] units_lang <thread|pool> <n>
#[cfg(debug_assertions)]
hirsel_guard::install!();
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pool = match a[1].as_str() {
        "thread" => false,
        "pool" => true,
        m => panic!("mode {m}: thread or pool"),
    };
    let n = a[2].parse().expect("a number");
    std::process::exit(hirsel_core_rt::units_lang_pingpong(pool, n).into());
}
