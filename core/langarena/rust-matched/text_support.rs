// Shared command line adapter for the text lane's crate-free matched twins.
// Runner protocol: TASK WARMUP ITERATIONS EXPECTED key=value ...
pub fn arguments(fields: &[&str], with_task: bool) -> Vec<String> {
    let raw: Vec<String> = std::env::args().collect();
    let mut positional = vec![raw[0].clone()];
    if with_task {
        positional.push(raw[1].clone());
    }
    for field in fields {
        positional.push(
            raw[5..]
                .iter()
                .find_map(|s| s.strip_prefix(&format!("{field}=")))
                .expect("missing field")
                .to_owned(),
        );
    }
    positional.push(raw[2].clone());
    positional.push(raw[3].clone());
    positional
}
pub fn output(checksum: u32) {
    let raw: Vec<String> = std::env::args().collect();
    let expected: u32 = raw[4].parse().unwrap();
    println!(
        "{} checksum={} expected={} status={}",
        raw[1],
        checksum,
        expected,
        if checksum == expected { "OK" } else { "ERR" }
    );
    if checksum != expected {
        std::process::exit(1);
    }
}
