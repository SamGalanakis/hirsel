use super::*;

/// PROG-9 identity bytes, one case per clause of the S rules, each expected value
/// derived by hand from programs.md (the first is the spec's own example, verbatim).
/// Bugs: the default mode `view` written; spaces around `->` or after `,`; a missing
/// result left out; `waits`, anchors or outlives pairs dropped, unsorted or repeated; a
/// generics list written, or region generics numbered by declared order rather than first
/// naming; an unescaped byte in a definition identity component.
#[test]
fn code_identity_bytes_follow_prog9() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "main",
            "fn double(b: Bytes) -> Result[Bytes, Capacity] { let mut out: Bytes = list.new(); list.append(mut out, b); list.append(mut out, b); Ok(out) }",
            "app@1/main/double(core@1/core/List[u8])->core@1/core/Result[core@1/core/List[u8],core@1/core/Capacity]",
        ),
        // Modes: `view` is the default and is not written; `mut` and `take` are words.
        // A missing result and a written `()` are both `->()`.
        (
            "main",
            "fn double(mut a: List[u8], take b: List[u8], c: u64) {}",
            "app@1/main/double(mut core@1/core/List[u8],take core@1/core/List[u8],u64)->()",
        ),
        ("main", "fn double() -> () {}", "app@1/main/double()->()"),
        // `waits` stays, as a word after the result.
        (
            "main",
            "fn add_one(take x: u64) -> u64 { x + 1 }\nfn double(tasks: Tasks) waits { let h = task.spawn(tasks, bytes 64, 41u64, add_one); let _ = result.unwrap(task.join(take h)) }",
            "app@1/main/double(core@1/core/Tasks)->()waits",
        ),
        // No generics list; a region generic is a root `in ^i`.
        (
            "main",
            "fn double[in s](root: Text in s, x: View[Text in s]) -> u64 { 0 }",
            "app@1/main/double(core@1/core/Text in ^0,core@1/core/View[core@1/core/Text in ^0])->u64",
        ),
        // The spec's second example, verbatim, in both declared orders: region generics
        // are numbered by first naming, parameters (Region ones too) are `#i`, and the
        // closure's pairs sort every `^` before every `#`.
        (
            "main",
            "fn edges[in b, in a](s: Region, r: Region, t: Text in a, u: Text in b) -> u64 where a: r, r: s { 0 }",
            "app@1/main/edges(core@1/core/Region,core@1/core/Region,core@1/core/Text in ^0,core@1/core/Text in ^1)->u64 where ^0:#0,^0:#1,#1:#0",
        ),
        (
            "main",
            "fn edges[in a, in b](s: Region, r: Region, t: Text in a, u: Text in b) -> u64 where a: r, r: s { 0 }",
            "app@1/main/edges(core@1/core/Region,core@1/core/Region,core@1/core/Text in ^0,core@1/core/Text in ^1)->u64 where ^0:#0,^0:#1,#1:#0",
        ),
        // A region generic named in neither the parameters nor the result takes the next
        // number: `b` is `^0`, `a` is `^1`.
        (
            "main",
            "fn double[in a, in b](x: Text in b) -> u64 where a: b { 0 }",
            "app@1/main/double(core@1/core/Text in ^0)->u64 where ^1:^0",
        ),
        // Two named in neither: declared order, `a` then `b`, whatever order the clauses
        // or their closure list them in (`a: c` by transitivity).
        (
            "main",
            "fn double[in a, in b, in c](x: Text in c) -> u64 where b: c, a: b { 0 }",
            "app@1/main/double(core@1/core/Text in ^0)->u64 where ^1:^0,^1:^2,^2:^0",
        ),
        // A root that is a parameter names it by position.
        (
            "main",
            "fn double(n: u64, r: Region, x: Text in r) -> u64 { n }",
            "app@1/main/double(u64,core@1/core/Region,core@1/core/Text in #1)->u64",
        ),
        // Outlives pairs sorted by `i` then `j`, each once.
        (
            "main",
            "fn double[in a, in b, in c](x: Text in a, y: Text in b, z: Text in c) -> u64 where c: b, a: b, a: b { 0 }",
            "app@1/main/double(core@1/core/Text in ^0,core@1/core/Text in ^1,core@1/core/Text in ^2)->u64 where ^0:^1,^2:^1",
        ),
        // A callable parameter: `own[send]`, its own modes and result, no names.
        (
            "main",
            "fn double(take k: own[send] fn(u64) -> u64, g: fn(mut u64)) -> u64 { k(1) }",
            "app@1/main/double(take own[send]fn(u64)->u64,fn(mut u64)->())->u64",
        ),
        // Every byte of a component other than a letter, digit or `_` is `%XX`, in the
        // function's identity and in every type path's.
        (
            "net-io",
            "type Doc { n: u64 }\nfn double(d: Doc) {}",
            "app@1/net%2Dio/double(app@1/net%2Dio/Doc)->()",
        ),
    ];
    let mut failures = vec![];
    for &(module, header, expected) in cases {
        let mut want = vec![1u8];
        want.extend_from_slice(expected.as_bytes());
        // The function under test is the one the expected identity names.
        let name = expected
            .split('/')
            .nth(2)
            .unwrap()
            .split('(')
            .next()
            .unwrap();
        let run = format!(
            "fn run() -> u64 {{\n    let _ = code.identity(code.describe({name}));\n    0\n}}\n"
        );
        let source = format!("{header}\n{run}pub fn main() -> Result[(), Exit] {{ Ok(()) }}\n");
        let (index, modules) = inputs_in(&source, &core_dir(), module);
        let program = Program::declare(&index, &modules);
        if !program.refusals.is_empty() || !program.gaps.is_empty() {
            failures.push(format!(
                "{header}: {:?} {:?}",
                program.refusals, program.gaps
            ));
            continue;
        }
        let target = program
            .fns
            .iter()
            .find(|fd| fd.path.package_is("app", "1") && fd.path.0.name == name)
            .unwrap();
        let direct = crate::identity::code_identity(&program.decls, &program.types, target, &[]);
        if direct.as_deref() != Some(&want[..]) {
            let got = direct.map(|b| String::from_utf8_lossy(&b[1..]).into_owned());
            failures.push(format!("{header}:\n  want {expected}\n  got  {got:?}"));
        }
        let mut found = false;
        for (f, fd) in program.fns.iter().enumerate() {
            if fd.body.is_none() || !fd.path.package_is("app", "1") {
                continue;
            }
            let mut emitter = Emitter::new();
            let check = program.check_lower(f, &mut emitter);
            if !check.refusals.is_empty() || !check.gaps.is_empty() {
                failures.push(format!("{header}: {:?} {:?}", check.refusals, check.gaps));
            }
            emitter.seal();
            found |= emitter
                .module
                .statics
                .iter()
                .any(|s| s.literal_bytes() == Some(&want));
        }
        if !found {
            failures.push(format!("{header}: code.identity does not read these bytes"));
        }
    }
    let instances = [
        (
            "fn pick[T: plain](a: T, b: T) -> T { a }",
            "fn arguments(a: u64) {}",
            "app@1/main/pick[u64]{plain[u64]}($0,$0)->$0",
        ),
        (
            "fn pick[T: plain](a: T, b: T) -> T { a }",
            "fn arguments(a: u32) {}",
            "app@1/main/pick[u32]{plain[u32]}($0,$0)->$0",
        ),
        (
            "fn swap[B, A: send](take p: (A, B)) -> (B, A) { let (a, b) = take p; (take b, take a) }",
            "fn arguments(b: Text, a: u64) {}",
            "app@1/main/swap[u64,core@1/core/Text]{send[u64]}(take($0,$1))->($1,$0)",
        ),
        (
            "fn swap[A: send, B](take p: (A, B)) -> (B, A) { let (a, b) = take p; (take b, take a) }",
            "fn arguments(a: u64, b: Text) {}",
            "app@1/main/swap[u64,core@1/core/Text]{send[u64]}(take($0,$1))->($1,$0)",
        ),
    ];
    for (header, arguments, expected) in instances {
        let source =
            format!("{header}\n{arguments}\npub fn main() -> Result[(), Exit] {{ Ok(()) }}\n");
        let (index, modules) = inputs_in(&source, &core_dir(), "main");
        let program = Program::declare(&index, &modules);
        assert!(program.refusals.is_empty(), "{:?}", program.refusals);
        assert!(program.gaps.is_empty(), "{:?}", program.gaps);
        let name = expected
            .split('/')
            .nth(2)
            .unwrap()
            .split('[')
            .next()
            .unwrap();
        let target = program
            .fns
            .iter()
            .find(|fd| fd.path.package_is("app", "1") && fd.path.0.name == name)
            .unwrap();
        let args = program
            .fns
            .iter()
            .find(|fd| fd.path.package_is("app", "1") && fd.path.0.name == "arguments")
            .unwrap();
        let args: Vec<_> = args.params.iter().map(|p| p.ty).collect();
        let want = [&[1][..], expected.as_bytes()].concat();
        let got = crate::identity::code_identity(&program.decls, &program.types, target, &args);
        if got.as_deref() != Some(&want) {
            failures.push(format!(
                "{header} at {arguments}:\n  want {expected}\n  got  {got:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
