use super::*;
use std::fmt::Write;

fn declared(source: &str) -> Program<'static> {
    let (index, modules) = crate::tests::program_inputs(source);
    let (index, modules) = (Box::leak(Box::new(index)), Box::leak(Box::new(modules)));
    Program::declare(index, modules)
}

/// The declaration each resource refusal names, and its message.
fn refused(p: &Program) -> Vec<(String, String)> {
    p.resource_refusals
        .iter()
        .map(|r| {
            let decl = p
                .decls
                .iter()
                .find(|d| d.site == r.site)
                .expect("a declaration's site");
            (decl.path.0.name.clone(), r.message.clone())
        })
        .collect()
}

fn accepted(p: &Program) {
    assert!(
        p.refusals.is_empty() && p.gaps.is_empty() && p.resource_refusals.is_empty(),
        "{:?} {:?} {:?}",
        p.refusals,
        p.gaps,
        p.resource_refusals
    );
}

fn chain(first: &str, line: impl Fn(usize) -> String, n: usize) -> String {
    let mut source = format!("{first}\n");
    for i in 1..=n {
        writeln!(source, "{}", line(i)).unwrap();
    }
    source
}

#[test]
fn doubling_tuple_alias_chain_is_refused_once_where_it_crosses() {
    // T{n} expands to 2^(n+1) - 1 nodes: T15 is 65,535, within; T16 is 131,071.
    let source = chain(
        "type T0 = u64",
        |n| format!("type T{n} = (T{p}, T{p})", p = n - 1),
        40,
    ) + "pub fn size() -> u64 { bytes[T40] }\n";
    let started = std::time::Instant::now();
    let p = declared(&source);
    let refused = refused(&p);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].0, "T16");
    assert!(
        refused[0].1.contains("expands to 131071 type nodes")
            && refused[0].1.contains("limit of 65536 nodes"),
        "{}",
        refused[0].1
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

#[test]
fn doubling_record_chain_is_refused_before_any_walk_of_its_fields() {
    // Before the limit, deriving T24's encoding walked all 2^25 of its nodes.
    let source = chain(
        "type T0 { a: u64, b: u64 }",
        |n| format!("type T{n} {{ a: T{p}, b: T{p} }}", p = n - 1),
        24,
    );
    let p = declared(&source);
    let refused = refused(&p);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].0, "T15");
    assert!(
        refused[0].1.contains("131071 type nodes"),
        "{}",
        refused[0].1
    );
}

#[test]
fn doubling_variant_and_generic_chains_are_refused() {
    let source = chain(
        "type V0 = A(u64, u64) | B",
        |n| format!("type V{n} = A(V{p}, V{p}) | B", p = n - 1),
        30,
    );
    let p = declared(&source);
    assert_eq!(refused(&p).len(), 1, "{:?}", p.resource_refusals);

    // A generic pair is small; its nesting at a closed argument is what grows.
    let mut source = String::from("type P[T] { a: T, b: T }\ntype G0 = P[u64]\n");
    for n in 1..=20 {
        writeln!(source, "type G{n} = P[G{p}]", p = n - 1).unwrap();
    }
    let p = declared(&source);
    let refused = refused(&p);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].0, "G15");
}

#[test]
fn nesting_past_the_depth_limit_is_refused_once() {
    let n = TYPE_DEPTH as usize + 40;
    // Declared outermost first, so counting the first declaration recurses its whole depth.
    let mut source = String::new();
    for i in 0..n {
        writeln!(source, "type D{i} {{ a: D{} }}", i + 1).unwrap();
    }
    writeln!(source, "type D{n} {{ a: u64 }}").unwrap();
    let p = declared(&source);
    let refused = refused(&p);
    assert_eq!(refused.len(), 1, "{refused:?}");
    // D{k} nests n + 2 - k types (its own node down to the u64); 257 is past 256.
    assert_eq!(
        refused[0].0,
        format!("D{}", n + 2 - (TYPE_DEPTH as usize + 1))
    );
    assert!(refused[0].1.contains("257 deep"), "{}", refused[0].1);
}

#[test]
fn nested_inline_past_the_frame_ceiling_is_refused_at_its_declaration() {
    let source = "type S256 { a: Inline[u64, 32] }
type S8K { a: Inline[S256, 32] }
type S256K { a: Inline[S8K, 32] }
type S8M { a: Inline[S256K, 32] }
type Pair { a: S8M, b: S8M }
";
    let p = declared(source);
    let refused = refused(&p);
    // Pair holds S8M, which is refused already.
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].0, "S8M");
    assert!(
        refused[0].1.contains("is 8388608 bytes")
            && refused[0]
                .1
                .contains(&format!("limit of {LAYOUT_BYTES} bytes")),
        "{}",
        refused[0].1
    );
}

#[test]
fn large_ordinary_types_are_within_every_limit() {
    // Exactly the frame ceiling, a 64-by-64 field record and a 200-deep chain.
    let mut source = format!("type Ceiling {{ a: Inline[u8, {LAYOUT_BYTES}] }}\n");
    source.push_str("type Row {");
    for i in 0..64 {
        write!(source, " f{i}: u64,").unwrap();
    }
    source.push_str(" }\ntype Grid {");
    for i in 0..64 {
        write!(source, " r{i}: Row,").unwrap();
    }
    source.push_str(" }\ntype Wide = (Grid, Grid, Grid, Grid)\n");
    for i in 0..200 {
        writeln!(source, "type D{i} {{ a: D{}, b: Option[Box[D0]] }}", i + 1).unwrap();
    }
    source.push_str("type D200 { a: Inline[Grid, 4] }\n");
    source.push_str(&chain(
        "type T0 = u64",
        |n| format!("type T{n} = (T{p}, T{p})", p = n - 1),
        14,
    ));
    let p = declared(&source);
    accepted(&p);
    let mut expansion = Expansion::new(&p.decls);
    let mut types = p.types.clone();
    let decl = p
        .decls
        .iter()
        .position(|d| d.path.0.name == "Wide")
        .unwrap();
    let DeclBody::Alias(wide) = p.decls[decl].body else {
        panic!("Wide is an alias")
    };
    assert_eq!(
        expansion.extent(&mut types, wide).unwrap(),
        Extent {
            nodes: 1 + 4 * (1 + 64 * (1 + 64)),
            depth: 4
        }
    );
}
