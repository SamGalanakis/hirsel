//! A chain of flow stages driven by one owner, on the calling thread or on
//! the worker pool. Usage:
//! [HIRSEL_UNTIED=<workers>] flow_pool <thread|pool> <stages> <items> <rounds>
#[cfg(debug_assertions)]
hirsel_guard::install!();
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pool = match a[1].as_str() {
        "thread" => false,
        "pool" => true,
        m => panic!("mode {m}: thread or pool"),
    };
    let num = |i: usize| a[i].parse::<u64>().expect("a number");
    let code = hirsel_core_rt::flow::bench::pool_pipeline(pool, num(2) as usize, num(3), num(4));
    std::process::exit(code.into());
}
