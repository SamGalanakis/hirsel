// The coverage tool laws (`docs/coverage.md`, "Laws": Round trip, Refusal,
// Diff, Raw file offline, Reached by, Denominator, Others and Shapes).

/// Coverage C4 "Merge" and "The profile" (`docs/coverage.md`), on two
/// profiles worked by hand. One build runs `t1` at seed 0, which passes and
/// reaches f's entry and `yes`, and `t2` unseeded, which fails and reaches
/// f's entry and `no` and g's entry, a site t1's map does not hold. Sorted
/// keys are f#e, f's `no`, f's `yes`, g#e (`#` sorts before `@`), so t1's
/// reached set is bits 0 and 2, byte 5, "BQ==", and t2's is bits 0, 1 and 3,
/// byte 11, "Cw==". The report reads back as the merged profile, and the
/// conflicts a merge refuses are refused. Against every other test ("Others"),
/// t1 alone reached f's `yes`, and only the others reached f's `no` and g#e.
#[test]
fn coverage_profiles_merge_read_back_and_refuse_conflicts() {
    use crate::coverage::{Build, Function, Place, Profile, Shape, SiteMap};
    let build = Build { engine: "cranelift".into(), optimize: false, target: "x86_64-unknown-linux-gnu".into() };
    let site = |function: &str, arm: Option<&str>, line: u64| crate::coverage::Site {
        arm: arm.map(str::to_owned),
        definition: function.into(),
        function: function.into(),
        place: Place { file: Some("main.hirsel".into()), line: Some(line), ..Place::default() },
        passed_runs: 0,
        failed_runs: 0,
        shape: None,
    };
    let (f, g) = ("app@1/main/f", "app@1/main/g");
    let (entry, no, yes, g_entry) = ("app@1/main/f#e", "app@1/main/f@10-15.0:no", "app@1/main/f@10-15.0:yes", "app@1/main/g#e");
    let sources = |pairs: &[(&str, &str)]| -> BTreeMap<String, Function> {
        pairs.iter().map(|(name, src)| ((*name).to_owned(), Function { src: Some((*src).to_owned()), place: Place::default() })).collect()
    };
    let run = |build: &Build, test: &str, seed, passed, flags: [bool; 3], extra: bool, f_src: &str, yes_line: u64| {
        let mut observed = vec![
            (entry.to_owned(), site(f, None, 3), flags[0]),
            (no.to_owned(), site(f, Some("no"), 4), flags[1]),
            (yes.to_owned(), site(f, Some("yes"), yes_line), flags[2]),
        ];
        let mut functions = vec![(f, f_src)];
        if extra {
            observed.push((g_entry.to_owned(), site(g, None, 8), true));
            functions.push((g, "src-g"));
        }
        let (mut profile, _) = Profile::of_run(build, test, seed, passed, Some(observed), true);
        profile.functions = sources(&functions);
        profile
    };
    let a = run(&build, "app@1/main/t1", Some(0), true, [true, false, true], false, "src-f", 4);
    let b = run(&build, "app@1/main/t2", None, false, [true, true, false], true, "src-f", 4);
    let mut merged = a.clone();
    merged.merge(b.clone()).expect("two runs of one build that share f merge");
    let document = merged.report(None, None).document;
    let counts: Vec<(&str, u64, u64)> = document["sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["site"].as_str().unwrap(), r["passed_runs"].as_u64().unwrap(), r["failed_runs"].as_u64().unwrap()))
        .collect();
    assert_eq!(counts, [(entry, 1, 1), (no, 0, 1), (yes, 1, 0), (g_entry, 0, 1)]);
    let tests: Vec<(&str, serde_json::Value, &str)> = document["tests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["definition"].as_str().unwrap(), r["seeds"].clone(), r["reached"].as_str().unwrap()))
        .collect();
    assert_eq!(tests, [("app@1/main/t1", serde_json::json!([[0, 0]]), "BQ=="), ("app@1/main/t2", serde_json::json!([null]), "Cw==")]);
    assert_eq!((&document["runs"], &document["failed_runs"], &document["hirsel_coverage"]), (&2.into(), &1.into(), &3.into()));
    assert_eq!(Profile::from_json(&document), Ok(merged.clone()));
    let law = PackageLaw::new();
    let path = law.path("merged.json");
    std::fs::write(&path, document.to_string()).unwrap();
    let others = |test: &str| law.tool(&["coverage", "diff", path.to_str().unwrap(), "--test", test, "--json"]);
    let (status, stdout, stderr) = others("app@1/main/t1");
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!((&json["others_only"], &json["test_only"]), (&serde_json::json!([no, g_entry]), &serde_json::json!([yes])));
    let (status, _, stderr) = others("app@1/main/t9");
    assert_eq!((status, stderr.ends_with("the profile holds no run of app@1/main/t9\n")), (Some(1), true), "{stderr}");

    // The same run on another engine is another run: C3's profiles merge.
    let llvm = Build { engine: "llvm".into(), ..build.clone() };
    let mut engines = merged.clone();
    engines.merge(run(&llvm, "app@1/main/t2", None, false, [true, true, false], true, "src-f", 4)).unwrap();
    assert_eq!((engines.runs, engines.tests.len()), (3, 3));

    let refusal = |other: Profile| merged.clone().merge(other).unwrap_err().0;
    assert_eq!(refusal(b.clone()), "the run of app@1/main/t2 on cranelift is in both profiles");
    assert_eq!(
        refusal(run(&build, "app@1/main/t3", Some(5), true, [true, false, false], false, "src-f2", 4)),
        "app@1/main/f has source src-f in one input and src-f2 in another: they are builds of different sources"
    );
    assert_eq!(
        refusal(run(&build, "app@1/main/t3", Some(5), true, [true, false, false], false, "src-f", 5)),
        "site app@1/main/f@10-15.0:yes is placed differently in two inputs: they are builds of different sources"
    );
    // One source whose arm target sits at different first positions in two
    // links' instances merges, keeping the earliest and its shape in either
    // order. On 2026-11-26 the later Store arm landed inside a fail call.
    let armed = |test: &str, line: u64, col: u64, shape: Option<Shape>| {
        let mut profile = run(&build, test, Some(5), true, [true, false, true], false, "src-f", 4);
        let site = profile.sites.get_mut(yes).unwrap();
        (site.place.arm_line, site.place.arm_col) = (Some(line), Some(col));
        site.shape = shape;
        profile
    };
    for (line, col, shape) in [(233, 26, None), (232, 15, Some(Shape::Fail))] {
        let ordinary = armed("app@1/main/t3", 232, 15, None);
        let fail = armed("app@1/main/t4", line, col, Some(Shape::Fail));
        for (mut placed, other) in [(ordinary.clone(), fail.clone()), (fail, ordinary)] {
            placed.merge(other).expect("arm positions of one source merge");
            let site = &placed.sites[yes];
            assert_eq!((site.place.arm_line, site.place.arm_col, site.shape), (Some(232), Some(15), shape));
        }
    }
    let mut old = document.clone();
    old["hirsel_coverage"] = 2.into();
    assert_eq!(Profile::from_json(&old).unwrap_err().0, "profile format 2; this hirsel reads format 3");
    let map = serde_json::json!({"hirsel_covmap": 1});
    assert_eq!(SiteMap::from_json(&map).unwrap_err().0, "site map format 1; this hirsel reads format 2");
}

const PICKS: &str = r#"
fn classify(s: u64) -> u64 {
    let mut out = 0u64
    if s == 1u64 { out = 1u64 }
    out
}
@test
fn picks() -> Result[(), ()] { if classify(INPUT) == INPUT { Ok(()) } else { Error(()) } }
fn unused(s: u64) -> u64 { s }
fn small(s: u64) -> Result[bool, ()] {
    if s > 5u64 { return Error(()) }
    match s { n if n < 30u64 => Ok(true), _ => Ok(false) }
}
fn shapes(s: u64) -> Result[u64, ()] { if small(s)? { Ok(s) } else { fail("big") } }
@test
fn shaped() -> Result[(), ()] { if shapes(1u64)? == 1u64 { Ok(()) } else { Error(()) } }
"#;

/// Coverage C4 "The coverage tool" (`docs/coverage.md`). Two sources differ
/// only in `picks`'s input: by hand, input 0 takes `classify`'s `no` and
/// input 1 its `yes`, so `new` gains `yes` and loses `no`, and `picks`, whose
/// source differs, is changed and has its own arms left out. The two
/// profiles are builds of different sources, so merging them is refused; one
/// profile through `coverage merge` reads back as itself; a raw file made by
/// hand, read against the `.covmap` the build wrote, gives the run it
/// records. Lines are PICKS's, the first being its leading break.
///
/// C7, by hand. Denominator: PICKS has six functions, so every profile's
/// function total is 6, whether one test ran (`picks` reaches two) or both.
/// Shapes: small's `s > 5u64` `yes` returns an Error (fail); shapes's
/// `small(s)?` is two decisions at one range, the `?` first (.0, whose `yes`
/// is the error arm) and the `if` reading it (.1, whose `no` is `fail`);
/// shaped's `?` is an error arm and its `==` has no shape. None of the four
/// is taken. small's guard `n < 30u64` has two guard arms, and 1 takes `yes`.
/// Reached by: of two raw runs, seed 7 flags `yes` and seed 8 `no`.
#[test]
fn coverage_tool_diffs_merges_and_reads_raw_files() {
    use crate::coverage::{Profile, SiteMap};
    let law = PackageLaw::new();
    law.manifest(
        "app",
        "app",
        1,
        "[sources]\n\"main.hirsel\" = \"main\"\n\n[dependencies]\nstd = { major = 1 }\n\n[tests]\nlimit = 1048576\n",
    );
    let (a, b) = (law.path("a.json"), law.path("b.json"));
    for (input, out) in [("0u64", &a), ("1u64", &b)] {
        law.write("app/main.hirsel", PICKS.replace("INPUT", input));
        let built = law.path("built");
        let (status, stdout, stderr) =
            law.run(&["test", "app", "--filter", "picks", "-o", built.to_str().unwrap(), "--coverage-out", out.to_str().unwrap()]);
        assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    }
    let doc = |path: &std::path::Path| -> serde_json::Value { serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap() };
    assert_eq!(doc(&a)["totals"]["functions"], serde_json::json!({"reached": 2, "total": 6}));
    let full = law.path("full.json");
    let (status, stdout, stderr) = law.run(&["test", "app", "--coverage-out", full.to_str().unwrap()]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    let full = doc(&full);
    assert_eq!(full["totals"]["functions"], serde_json::json!({"reached": 5, "total": 6}));
    let none = serde_json::json!({"reached": 0, "total": 2});
    assert_eq!(full["totals"]["shapes"], serde_json::json!({"error": none, "fail": none, "guard": {"reached": 1, "total": 2}}));
    // (function, range length, ordinal, arm, shape), by key.
    let mut shaped: Vec<(String, u64, u64, String, Option<String>)> = Vec::new();
    for row in full["sites"].as_array().unwrap() {
        let key = row["site"].as_str().unwrap();
        let Some((head, arm)) = key.rsplit_once(':') else { continue };
        let (head, ordinal) = head.rsplit_once('.').unwrap();
        let (function, range) = head.rsplit_once('@').unwrap();
        let (start, end) = range.split_once('-').unwrap();
        if ["app@1/main/small", "app@1/main/shapes", "app@1/main/shaped"].contains(&function) {
            let length = end.parse::<u64>().unwrap() - start.parse::<u64>().unwrap();
            let shape = row["shape"].as_str().map(str::to_owned);
            shaped.push((function["app@1/main/".len()..].to_owned(), length, ordinal.parse().unwrap(), arm.to_owned(), shape));
        }
    }
    shaped.sort();
    let row = |f: &str, text: &str, n, arm: &str, shape: Option<&str>| (f.to_owned(), text.len() as u64, n, arm.to_owned(), shape.map(str::to_owned));
    assert_eq!(shaped, [
        row("shaped", "shapes(1u64)?", 0, "no", None),
        row("shaped", "shapes(1u64)?", 0, "yes", Some("error")),
        row("shaped", "shapes(1u64)? == 1u64", 0, "no", None),
        row("shaped", "shapes(1u64)? == 1u64", 0, "yes", None),
        row("shapes", "small(s)?", 0, "no", None),
        row("shapes", "small(s)?", 0, "yes", Some("error")),
        row("shapes", "small(s)?", 1, "no", Some("fail")),
        row("shapes", "small(s)?", 1, "yes", None),
        row("small", "s > 5u64", 0, "no", None),
        row("small", "s > 5u64", 0, "yes", Some("fail")),
        row("small", "n < 30u64", 0, "no", Some("guard")),
        row("small", "n < 30u64", 0, "yes", Some("guard")),
    ]);
    let item = &PICKS[PICKS.find("fn classify").unwrap()..];
    let start = item.find("s == 1u64").unwrap();
    let decision = format!("app@1/main/classify@{start}-{}.0", start + "s == 1u64".len());
    let (a, b) = (a.to_str().unwrap(), b.to_str().unwrap());

    let (status, stdout, stderr) = law.tool(&["coverage", "diff", a, b, "--json"]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    let diff: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(diff["gained"], serde_json::json!([format!("{decision}:yes")]));
    assert_eq!(diff["lost"], serde_json::json!([format!("{decision}:no")]));
    assert_eq!(diff["changed_functions"], serde_json::json!(["app@1/main/picks"]));
    assert_eq!((&diff["reached"]["before"], &diff["reached"]["after"], &diff["selection_changed"]), (&2.into(), &2.into(), &false.into()));
    let (status, stdout, _) = law.tool(&["coverage", "diff", a, b]);
    assert_eq!((status, stdout.lines().next()), (Some(0), Some("since base  +1 sites, -1   1 functions changed")));

    let (status, stdout, stderr) = law.tool(&["coverage", "merge", a, b]);
    assert_eq!(status, Some(1), "{stdout}\n{stderr}");
    assert!(stderr.contains("app@1/main/picks has source") && stderr.ends_with("they are builds of different sources\n"), "{stderr}");
    let read = |path: &str| Profile::from_json(&serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()).unwrap();
    let merged = law.path("merged.json");
    let (status, stdout, stderr) = law.tool(&["coverage", "merge", a, "--json", "-o", merged.to_str().unwrap()]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    assert_eq!(Profile::from_json(&serde_json::from_str(&stdout).unwrap()), Ok(read(a)));
    assert_eq!(read(merged.to_str().unwrap()), read(a));

    // Input 1's build's `.covmap` for picks places classify's arms by hand.
    let mut maps = Vec::new();
    let mut dirs = vec![law.path("built")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap().map(Result::unwrap) {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "covmap") {
                maps.push(path);
            }
        }
    }
    let read_map = |path: &std::path::PathBuf| SiteMap::from_json(&serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()).unwrap();
    let map_path = maps.iter().find(|path| read_map(path).test == "app@1/main/picks").unwrap_or_else(|| panic!("{maps:?}"));
    let map = read_map(map_path);
    let (id, sites) = map.records.iter().find(|(_, sites)| sites[0].0 == "app@1/main/classify#e").unwrap();
    let keys: Vec<&str> = sites.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys, ["app@1/main/classify#e".to_owned(), format!("{decision}:no"), format!("{decision}:yes")]);
    for (key, site) in &sites[1..] {
        let place = &site.place;
        assert_eq!((place.file.as_deref(), place.line, place.col, place.end_line, place.end_col), (Some("main.hirsel"), Some(4), Some(8), Some(4), Some(17)), "{key}");
    }
    assert_eq!((sites[1].1.place.arm_line, sites[2].1.place.arm_line), (None, Some(4)));

    // A raw file ("The raw file") with classify's record flagged entry and
    // `yes`, seed 7 and a pass; picks's record is absent, so none of its
    // sites is reached.
    let header: [&[u8]; 10] = [b"HIRSELCV", &1u32.to_le_bytes(), &[0; 4], &56u64.to_le_bytes(), &48u64.to_le_bytes(), &1u32.to_le_bytes(), &1u32.to_le_bytes(), &7u64.to_le_bytes(), &0x4544_4F4Eu32.to_le_bytes(), &[0; 4]];
    let mut raw = [&header.concat()[..], &0x5643_4948u32.to_le_bytes(), &3u32.to_le_bytes(), &(0..32).map(|i| u8::from_str_radix(&id[2 * i..2 * i + 2], 16).unwrap()).collect::<Vec<u8>>(), &[1, 0, 1, 0, 0, 0, 0, 0]].concat();
    let raw_path = law.path("run.raw");
    std::fs::write(&raw_path, &raw).unwrap();
    let (status, stdout, stderr) =
        law.tool(&["coverage", "merge", raw_path.to_str().unwrap(), "--map", map_path.to_str().unwrap(), "--json"]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    let profile: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let test = &profile["tests"][0];
    assert_eq!((&test["definition"], &test["seeds"], &test["runs"], &test["failed_runs"]), (&"app@1/main/picks".into(), &serde_json::json!([[7, 7]]), &1.into(), &0.into()));
    let passed: BTreeMap<&str, u64> = profile["sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["site"].as_str().unwrap(), r["passed_runs"].as_u64().unwrap()))
        .filter(|(key, _)| key.starts_with("app@1/main/classify"))
        .collect();
    let yes = format!("{decision}:yes");
    let no = format!("{decision}:no");
    assert_eq!(passed, BTreeMap::from([("app@1/main/classify#e", 1), (no.as_str(), 0), (yes.as_str(), 1)]));
    let (status, _, stderr) = law.tool(&["coverage", "merge", raw_path.to_str().unwrap()]);
    assert_eq!((status, stderr.ends_with("a raw file needs --map\n")), (Some(1), true), "{stderr}");

    // As a directory or an `@LIST`: beside seed 7's pass, seed 8 killed before
    // its outcome (0) reads as failed, and it flags entry and `no`.
    let (runs, list) = (law.path("runs"), law.path("runs.list"));
    std::fs::create_dir(&runs).unwrap();
    std::fs::write(runs.join("a.7.raw"), &raw).unwrap();
    raw[36..48].copy_from_slice(&[[0; 4], [8, 0, 0, 0], [0; 4]].concat());
    let flags = raw.len() - 8;
    raw[flags..flags + 3].copy_from_slice(&[1, 1, 0]);
    std::fs::write(runs.join("a.8.raw"), &raw).unwrap();
    std::fs::write(&list, format!("{}\n{}\n", runs.join("a.7.raw").display(), runs.join("a.8.raw").display())).unwrap();
    for input in [runs.to_str().unwrap().to_owned(), format!("@{}", list.display())] {
        let (status, stdout, stderr) = law.tool(&["coverage", "merge", &input, "--map", map_path.to_str().unwrap(), "--json"]);
        let profile: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("{status:?}\n{stderr}"));
        assert_eq!((&profile["tests"][0]["seeds"], &profile["runs"], &profile["failed_runs"]), (&serde_json::json!([[7, 8]]), &2.into(), &1.into()), "{input}");
        let by: BTreeMap<&str, &serde_json::Value> = profile["sites"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["site"].as_str().unwrap().starts_with("app@1/main/classify"))
            .map(|r| (r["site"].as_str().unwrap(), &r["reached_by"]))
            .collect();
        let seeds = |range: [u64; 2]| serde_json::json!([{"definition": "app@1/main/picks", "build": 0, "seeds": [range]}]);
        assert_eq!(by, BTreeMap::from([("app@1/main/classify#e", &seeds([7, 8])), (no.as_str(), &seeds([8, 8])), (yes.as_str(), &seeds([7, 7]))]), "{input}");
    }
}
