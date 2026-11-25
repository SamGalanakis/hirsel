//! The runtime's C headers, compiled by the pinned clang, against the
//! declarations Hirsel's backend emits for the same `hirsel_core_*` symbols.
//! Clang is the oracle for each parameter's and result's lowered type
//! (register class), `signext`/`zeroext`, `inreg`, `sret`/`byval` position
//! and `align`.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Slot {
    ty: String,
    ext: &'static str,
    sret: bool,
    byval: bool,
    inreg: bool,
    align: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Decl {
    ret: Slot,
    params: Vec<Slot>,
}

/// Splits `s` at commas outside (), {}, [] and <>.
fn split_top(s: &str) -> Vec<&str> {
    let (mut depth, mut start, mut out) = (0i32, 0, Vec::new());
    for (i, c) in s.char_indices() {
        match c {
            '(' | '{' | '[' | '<' => depth += 1,
            ')' | '}' | ']' | '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    if !s[start..].trim().is_empty() {
        out.push(s[start..].trim());
    }
    out
}

fn type_start(word: &str) -> bool {
    matches!(word, "void" | "ptr" | "float" | "double" | "half")
        || word.starts_with(['{', '[', '<'])
        || (word.starts_with('i') && word[1..].parse::<u32>().is_ok())
}

/// The type and attributes of one parameter (`ty attrs...`) or of a result
/// (`attrs... ty`).
fn slot(text: &str, result: bool) -> Slot {
    let words: Vec<&str> = text.split_whitespace().collect();
    let at = if result {
        words.iter().position(|w| type_start(w)).unwrap_or(0)
    } else {
        0
    };
    let rest = words[at..].join(" ");
    let (ty, attrs) = match rest.chars().next() {
        Some(open @ ('{' | '[' | '<')) => {
            let close = match open {
                '{' => '}',
                '[' => ']',
                _ => '>',
            };
            let end = rest.find(close).unwrap() + 1;
            (rest[..end].to_owned(), rest[end..].to_owned())
        }
        _ => {
            let (t, a) = rest.split_once(' ').unwrap_or((&rest, ""));
            (t.to_owned(), a.to_owned())
        }
    };
    let all = format!("{} {attrs}", words[..at].join(" "));
    let has = |a: &str| all.split_whitespace().any(|w| w == a);
    let align = all
        .split_whitespace()
        .skip_while(|w| *w != "align")
        .nth(1)
        .and_then(|n| n.parse().ok());
    Slot {
        ty,
        ext: if has("zeroext") {
            "zeroext"
        } else if has("signext") {
            "signext"
        } else {
            ""
        },
        sret: all.contains("sret("),
        byval: all.contains("byval("),
        inreg: has("inreg"),
        align,
    }
}

/// Every `declare` of a `hirsel_core_*` symbol in `ir`.
fn declarations(ir: &str, into: &mut BTreeMap<String, Decl>) {
    for line in ir.lines().filter(|l| l.starts_with("declare ")) {
        // Test hooks are defined by each test's C harness, not the runtime.
        let Some(at) = line
            .find("@hirsel_core_")
            .filter(|&at| !line[at..].starts_with("@hirsel_core_test_"))
        else {
            continue;
        };
        let open = at + line[at..].find('(').unwrap();
        let mut depth = 0;
        let close = open
            + line[open..]
                .char_indices()
                .find(|&(_, c)| {
                    depth += match c {
                        '(' => 1,
                        ')' => -1,
                        _ => 0,
                    };
                    depth == 0
                })
                .unwrap()
                .0;
        let name = line[at + 1..open].trim_matches('"').to_owned();
        let decl = Decl {
            ret: slot(&line["declare ".len()..at], true),
            params: split_top(&line[open + 1..close])
                .into_iter()
                .filter(|p| *p != "...")
                .map(|p| slot(p, false))
                .collect(),
        };
        into.insert(name, decl);
    }
}

fn clang_declarations(triple: &str) -> BTreeMap<String, Decl> {
    let prefix =
        PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").expect("pinned LLVM prefix"));
    let include = std::fs::canonicalize(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hirsel-core-rt/include"),
    )
    .unwrap();
    let mut headers: Vec<_> = std::fs::read_dir(&include)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "h"))
        .collect();
    headers.sort();
    let mut source = String::new();
    let mut names = std::collections::BTreeSet::new();
    for header in &headers {
        source.push_str(&format!("#include \"{}\"\n", header.display()));
        let text = std::fs::read_to_string(header).unwrap();
        for (i, _) in text.match_indices("hirsel_core_") {
            let name: String = text[i..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if text[i + name.len()..].starts_with('(') {
                names.insert(name);
            }
        }
    }
    source.push_str("void *hirsel_keep[] = {\n");
    for name in &names {
        source.push_str(&format!("(void *)&{name},\n"));
    }
    source.push_str("};\n");
    let scratch = host_memory::NativeArtifacts::new();
    let c = scratch.path().join("keep.c");
    std::fs::write(&c, source).unwrap();
    let run = Command::new(prefix.join("bin/clang"))
        .args([
            "-target",
            triple,
            "-ffreestanding",
            "-O0",
            "-S",
            "-emit-llvm",
            "-o",
            "-",
        ])
        .arg(&c)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{triple}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let mut out = BTreeMap::new();
    declarations(&String::from_utf8(run.stdout).unwrap(), &mut out);
    out
}

/// The declarations Hirsel emits for `target` across the Core samples and
/// one call of every provider row.
fn hirsel_declarations(target: Target) -> BTreeMap<String, Decl> {
    let samples = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hirsel-core-ir/samples");
    let mut sources: Vec<String> = std::fs::read_dir(samples)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "core"))
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect();
    sources.extend(HostOp::ALL.iter().map(|&op| host_abi::provider_source(op)));
    let mut out = BTreeMap::new();
    for source in sources {
        let Ok(module) = text::parse(&source) else {
            continue;
        };
        if wf::module(&module).is_err() {
            continue;
        }
        declarations(
            &emit_ir_for_target(&module, target, false, &[]).unwrap(),
            &mut out,
        );
    }
    assert!(
        out.len() > 50,
        "{target:?}: only {} runtime declarations",
        out.len()
    );
    out
}

/// Clang's register class for one lowered type: aggregates compare by their
/// flattened element classes, scalars by exact type.
fn class(ty: &str) -> String {
    // C `_Bool` is a one-byte INTEGER value that LLVM spells `i1`; its
    // extension is compared on its own.
    if ty == "i1" {
        return "i8".to_owned();
    }
    if !ty.starts_with(['{', '[']) {
        return ty.to_owned();
    }
    let mut out = Vec::new();
    let inner = &ty[1..ty.len() - 1];
    if ty.starts_with('[') {
        let (n, elem) = inner.split_once(" x ").unwrap();
        let n: usize = n.trim().parse().unwrap();
        out.extend(std::iter::repeat_n(class(elem.trim()), n));
    } else {
        out.extend(split_top(inner).into_iter().map(class));
    }
    let word = |t: &str| match t {
        "float" | "double" => t.to_owned(),
        _ => "int".to_owned(),
    };
    out.iter().map(|t| word(t)).collect::<Vec<_>>().join(",")
}

/// Every difference between Hirsel's and clang's declaration of a symbol.
fn mismatches(hirsel: &BTreeMap<String, Decl>, clang: &BTreeMap<String, Decl>) -> Vec<String> {
    let mut out = Vec::new();
    for (name, ours) in hirsel {
        let Some(theirs) = clang.get(name) else {
            out.push(format!("{name}: no C prototype in hirsel-core-rt/include"));
            continue;
        };
        if ours.params.len() != theirs.params.len() {
            out.push(format!(
                "{name}: {} parameters, clang {}",
                ours.params.len(),
                theirs.params.len()
            ));
            continue;
        }
        let slots = std::iter::once(("result".to_owned(), &ours.ret, &theirs.ret)).chain(
            ours.params
                .iter()
                .zip(&theirs.params)
                .enumerate()
                .map(|(i, (a, b))| (format!("arg{i}"), a, b)),
        );
        for (at, a, b) in slots {
            let mut diff = Vec::new();
            if !b.sret && !b.byval && class(&a.ty) != class(&b.ty) {
                diff.push(format!("type {} vs {}", a.ty, b.ty));
            }
            for (what, x, y) in [
                ("sret", a.sret, b.sret),
                ("byval", a.byval, b.byval),
                ("inreg", a.inreg, b.inreg),
            ] {
                if x != y {
                    diff.push(format!("{what} {x} vs {y}"));
                }
            }
            if a.ext != b.ext {
                diff.push(format!("ext '{}' vs '{}'", a.ext, b.ext));
            }
            if (b.sret || b.byval)
                && b.align.is_some()
                && a.align.is_some_and(|x| Some(x) != b.align)
            {
                diff.push(format!("align {:?} vs {:?}", a.align, b.align));
            }
            if !diff.is_empty() {
                out.push(format!(
                    "{name} {at}: {} (hirsel vs clang)",
                    diff.join("; ")
                ));
            }
        }
    }
    out
}

#[test]
fn runtime_declarations_match_clang() {
    let mut failures = Vec::new();
    for target in Target::ALL {
        let triple = target.triple();
        for m in mismatches(&hirsel_declarations(target), &clang_declarations(triple)) {
            failures.push(format!("{triple}: {m}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{}\n{}",
        failures.len(),
        failures.join("\n")
    );
}
