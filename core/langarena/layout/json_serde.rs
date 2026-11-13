// Idiomatic twin: serde_json 1.0.154, default map configuration; input has no duplicate
// keys or object-order ambiguity. Same file, 32 iterations, UTF-8 admission and checksum.
fn main() {
    let phase = std::env::args().nth(1).expect("phase");
    let input = std::fs::read_to_string(std::env::args().nth(2).expect("input")).unwrap();
    let sum: u64 = if phase == "json_parse" {
        (0..32)
            .map(|_| {
                serde_json::from_str::<serde_json::Value>(&input)
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len() as u64
            })
            .sum()
    } else if phase == "json_print" {
        let value: serde_json::Value = serde_json::from_str(&input).unwrap();
        (0..32)
            .map(|_| {
                serde_json::to_string(&value)
                    .unwrap()
                    .bytes()
                    .map(u64::from)
                    .sum::<u64>()
            })
            .sum()
    } else {
        panic!("unknown phase")
    };
    println!("{sum}");
}
