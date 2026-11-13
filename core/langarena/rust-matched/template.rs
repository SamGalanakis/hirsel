// Reviewed-by: langs-numeric-920 2026-11-13; Parse/Regex: same ASCII fixture, owned String hash map, byte matcher, and checked sizes; overflow checks required.
// Matched twin of template.hirsel. Reviewed below.
// Source: kostya/LangArena 3c9f84d benchmarks/template.rs:49-216.
// Same owned String keys/values, hash lookup, ASCII fixture and hand matcher.
use std::collections::HashMap;
fn render(source: &str, vars: &HashMap<String, String>, regex: bool) -> String {
    let b = source.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n.checked_add(n / 2).unwrap());
    let mut i = 0usize;
    while i < n {
        if i + 1 < n && b[i] == b'{' && b[i + 1] == b'{' {
            let mut j = i + 2;
            while j + 1 < n {
                if b[j] == b'}' && b[j + 1] == b'}' || regex && b[j] == b'\n' {
                    break;
                }
                j += 1;
            }
            if j + 1 < n && b[j] == b'}' && b[j + 1] == b'}' {
                let key = source[i + 2..j].trim_ascii();
                if let Some(value) = vars.get(key) {
                    out.push_str(value);
                }
                i = j + 2;
                continue;
            }
        }
        out.push(char::from(b[i]));
        i += 1;
    }
    out
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let task = &args[1];
    assert!(task == "Template::Parse" || task == "Template::Regex");
    let count: u64 = args[2].parse().unwrap();
    let warmup: u64 = args[3].parse().unwrap();
    let iterations: u64 = args[4].parse().unwrap();
    let first = [
        "John", "Jane", "Bob", "Alice", "Charlie", "Diana", "Sarah", "Mike",
    ];
    let last = [
        "Smith", "Johnson", "Brown", "Taylor", "Wilson", "Davis", "Miller", "Jones",
    ];
    let cities = [
        "New York",
        "Los Angeles",
        "Chicago",
        "Houston",
        "Phoenix",
        "San Francisco",
    ];
    let mut vars = HashMap::new();
    vars.insert("TITLE".to_owned(), "Template title".to_owned());
    let mut source = "<html><body><h1>{{TITLE}}</h1><p>Lorem {ipsum} dolor {sit} amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore {et} dolore magna aliqua. </p><table>".to_owned();
    for i in 0..count {
        if i % 3 == 0 {
            source.push_str("<!-- {comment} -->");
        }
        source.push_str(&format!("<tr><td>{{{{ FIRST_NAME{i} }}}}</td><td>{{{{LAST_NAME{i}}}}}</td><td>{{{{  CITY{i}  }}}}</td><td>{{balance: {}}}</td></tr>\n", i % 100));
        vars.insert(format!("FIRST_NAME{i}"), first[(i % 8) as usize].to_owned());
        vars.insert(format!("LAST_NAME{i}"), last[(i % 8) as usize].to_owned());
        vars.insert(format!("CITY{i}"), cities[(i % 6) as usize].to_owned());
    }
    source.push_str("</table></body></html>");
    let mut rendered = String::new();
    let mut checksum = 0u32;
    for _ in 0..warmup.checked_add(iterations).unwrap() {
        rendered = render(&source, &vars, task == "Template::Regex");
        checksum = checksum.wrapping_add(u32::try_from(rendered.len()).unwrap());
    }
    let hash = rendered.bytes().fold(5381u32, |h, b| {
        (h << 5).wrapping_add(h).wrapping_add(u32::from(b))
    });
    println!("{}", checksum.wrapping_add(hash));
}
