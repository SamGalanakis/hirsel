use super::*;

/// One step of a stat-trust case: at `at` on the test clock, the file holds
/// `bytes` stamped `time` (mtime and ctime), and a request asks for the
/// recipe's inputs; `during` runs between that request's read and its
/// classification.
struct Step {
    at: u64,
    time: (i64, i64),
    bytes: &'static [u8],
    during: Option<(&'static [u8], u64)>,
    /// The boot id this request runs under; the record's when unset.
    boot: Option<Option<&'static str>>,
    expect: RecipeInputs,
    reads: usize,
}

fn step(
    at: u64,
    time: (i64, i64),
    bytes: &'static [u8],
    expect: RecipeInputs,
    reads: usize,
) -> Step {
    Step {
        at,
        time,
        bytes,
        during: None,
        boot: None,
        expect,
        reads,
    }
}

const BOOT: Option<&str> = Some("3a0fcf86-35cb-41c6-8348-cdf6518ffb7a\n");
const OTHER_BOOT: Option<&str> = Some("5d2e9a41-0b7c-4f3e-9a8d-61c0ffee0042\n");
const NAMESPACE: Option<&str> = Some("time:[4026531834]");

/// Record the file as `first` bytes at clock 0, stamped `time`, under boot
/// id `boot` and namespace `namespace`; then run `steps` in order. A step
/// stamped like the record asserts that its stat really collides with the
/// recorded one. Returns the first step that went otherwise.
fn stat_trust_in(
    case: &str,
    (boot, namespace): (Option<&'static str>, Option<&'static str>),
    time: (i64, i64),
    first: &'static [u8],
    steps: Vec<Step>,
) -> Result<(), String> {
    SOURCE.set(Some((boot, namespace)));
    let result = stat_trust_steps(case, time, first, steps, namespace);
    SOURCE.set(None);
    CLOCK.set(None);
    TIMES.set(None);
    result
}

fn stat_trust(
    case: &str,
    time: (i64, i64),
    first: &'static [u8],
    steps: Vec<Step>,
) -> Result<(), String> {
    stat_trust_in(case, (BOOT, NAMESPACE), time, first, steps)
}

fn stat_trust_steps(
    case: &str,
    time: (i64, i64),
    first: &'static [u8],
    steps: Vec<Step>,
    namespace: Option<&'static str>,
) -> Result<(), String> {
    let work = tempfile::tempdir().unwrap();
    let input = work.path().join("crt.o");
    std::fs::write(&input, first).unwrap();
    CLOCK.set(Some(0));
    TIMES.set(Some(time));
    let mut recipe = recipe(&input);
    let recorded = recipe.files[0].stat.clone();
    for (index, step) in steps.into_iter().enumerate() {
        std::fs::write(&input, step.bytes).unwrap();
        TIMES.set(Some(step.time));
        if step.time == time {
            assert_eq!(
                recipe_stat(&input).unwrap(),
                recorded,
                "{case} step {index}: no collision"
            );
        }
        CLOCK.set(Some(step.at));
        if let Some(boot) = step.boot {
            SOURCE.set(Some((boot, namespace)));
        }
        if let Some((bytes, at)) = step.during {
            let input = input.clone();
            AFTER_READ.set(Some(Box::new(move || {
                std::fs::write(&input, bytes).unwrap();
                CLOCK.set(Some(at));
            })));
        }
        READS.set(0);
        let inputs = refresh_recipe_inputs(&mut recipe).unwrap();
        AFTER_READ.set(None);
        let got = (inputs, READS.get());
        let want = (step.expect, step.reads);
        if got != want {
            return Err(format!("{case} step {index}: got {got:?}, want {want:?}"));
        }
    }
    Ok(())
}

#[test]
fn a_stat_stands_for_bytes_only_after_two_sightings_a_window_apart() {
    use RecipeInputs::{Changed, Current, Restated};
    let tick = (1_700_000_000, 4_000_000);
    let later = (1_700_000_001, 4_000_000);
    let earlier = (1_699_999_999, 4_000_000);
    let future = (4_102_444_800, 0); // 2100-01-01, ahead of any clock here
    let ms = 1_000_000;
    let mut results = vec![];
    results.push(stat_trust(
        "same-tick equal-length edit",
        tick,
        b"first",
        vec![step(ms, tick, b"later", Changed, 1)],
    ));
    results.push(stat_trust(
        "edit after promotion in a later tick",
        tick,
        b"first",
        vec![
            step(WINDOW, tick, b"first", Restated, 1),
            step(WINDOW + ms, tick, b"first", Current, 0),
            step(WINDOW + 2 * ms, later, b"later", Changed, 1),
        ],
    ));
    results.push(stat_trust(
        "sightings exactly a window apart",
        tick,
        b"first",
        vec![
            step(WINDOW, tick, b"first", Restated, 1),
            step(WINDOW + ms, tick, b"first", Current, 0),
        ],
    ));
    results.push(stat_trust(
        "sightings just under a window apart",
        tick,
        b"first",
        vec![
            step(WINDOW - 1, tick, b"first", Current, 1),
            step(WINDOW + 5 * ms, tick, b"later", Changed, 1),
        ],
    ));
    results.push(stat_trust(
        "an edit between sightings in the recorded tick",
        tick,
        b"first",
        vec![step(WINDOW, tick, b"later", Changed, 1)],
    ));
    results.push(stat_trust(
        "a ctime in the future",
        future,
        b"first",
        vec![step(ms, future, b"later", Changed, 1)],
    ));
    results.push(stat_trust(
        "an edit after the promoting read",
        tick,
        b"first",
        vec![
            Step {
                during: Some((b"later", WINDOW + 10 * ms)),
                ..step(WINDOW - 1, tick, b"first", Current, 1)
            },
            step(WINDOW + 20 * ms, tick, b"later", Changed, 1),
        ],
    ));
    results.push(stat_trust(
        "a clock set back: an edit stamped before the settled stat",
        tick,
        b"first",
        vec![
            step(WINDOW, tick, b"first", Restated, 1),
            step(WINDOW + ms, earlier, b"later", Changed, 1),
        ],
    ));
    // By contract (README, "Tool inputs"): an edit that repeats the settled
    // stat exactly is trusted unread. Closing it means hashing every tool on
    // every request: about 230 MB, 615 M instructions against a no-change
    // request's 56 M.
    results.push(stat_trust(
        "by contract: an edit repeating the settled stat exactly",
        tick,
        b"first",
        vec![
            step(WINDOW, tick, b"first", Restated, 1),
            step(WINDOW + ms, tick, b"later", Current, 0),
        ],
    ));
    let failed: Vec<String> = results.into_iter().filter_map(Result::err).collect();
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
fn a_settled_stat_holds_in_another_boot_and_a_sighting_does_not() {
    use RecipeInputs::{Current, Restated};
    let tick = (1_700_000_000, 4_000_000);
    let ms = 1_000_000;
    let failed: Vec<String> = [
        stat_trust(
            "settled in one boot, read in another and without a clock",
            tick,
            b"first",
            vec![
                step(WINDOW, tick, b"first", Restated, 1),
                Step {
                    boot: Some(OTHER_BOOT),
                    ..step(ms, tick, b"first", Current, 0)
                },
                Step {
                    boot: Some(None),
                    ..step(2 * ms, tick, b"first", Current, 0)
                },
            ],
        ),
        stat_trust(
            "sighted in one boot, read a window later in another",
            tick,
            b"first",
            vec![
                Step {
                    boot: Some(OTHER_BOOT),
                    ..step(WINDOW, tick, b"first", Restated, 1)
                },
                step(WINDOW + ms, tick, b"first", Current, 1),
            ],
        ),
        stat_trust_in(
            "an empty boot id is no clock",
            (Some("\n"), NAMESPACE),
            tick,
            b"first",
            vec![
                step(WINDOW, tick, b"first", Current, 1),
                step(3 * WINDOW, tick, b"first", Current, 1),
            ],
        ),
        stat_trust_in(
            "a malformed boot id is no clock",
            (Some("not-a-boot-id"), NAMESPACE),
            tick,
            b"first",
            vec![step(WINDOW, tick, b"first", Current, 1)],
        ),
        stat_trust_in(
            "an unreadable time namespace is no clock",
            (BOOT, None),
            tick,
            b"first",
            vec![step(WINDOW, tick, b"first", Current, 1)],
        ),
    ]
    .into_iter()
    .filter_map(Result::err)
    .collect();
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
fn a_record_written_by_the_earlier_stat_rule_is_not_read() {
    // e4b65f6c recorded stats it could not trust. A manifest it wrote names
    // runtime bytes that the file no longer holds, under the live stat; it
    // is shaped like a current record, so only its schema keeps it out.
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let linker = bin.join("ld.lld");
    let crt = work.path().join("crt1.o");
    std::fs::write(&linker, b"linker").unwrap();
    std::fs::write(&crt, b"crt").unwrap();
    let cc = bin.join("cc");
    crate::executable::write(
        &cc,
        format!(
            "#!/bin/sh\necho '\"{}\" \"-pie\" \"-o\" \"out\" \"{}\" \"-lc\"' >&2\n",
            linker.display(),
            crt.display()
        )
        .as_bytes(),
        0o755,
    )
    .unwrap();
    let runtime = work.path().join("runtime.a");
    std::fs::write(&runtime, b"runtime").unwrap();
    let target = "x86_64-unknown-linux-gnu";
    let key = capture_key(&cc);
    let stale = |path: &Path| {
        serde_json::json!({
            "path": path,
            "content": ContentHash::of_bytes(b"bytes the file held before"),
            "stat": recipe_stat(path).unwrap(),
            "trust": "Settled",
        })
    };
    let manifest = serde_json::json!({
        "schema": 1,
        "target": target,
        "files": [stale(&cc.canonicalize().unwrap()), stale(&runtime), stale(&linker), stale(&crt)],
        "identity": ContentHash::of_bytes(b"identity"),
    });
    write_record(
        &recipes().unwrap(),
        toolchain_key(target, &key).unwrap(),
        &manifest,
    )
    .unwrap();
    assert_eq!(
        driver_toolchain(&cc, target, &runtime).unwrap().0.runtime,
        ContentHash::of_parts(
            "hirsel.core-runtime.v2",
            [ContentHash::of_bytes(b"runtime").0.as_slice()]
        ),
    );
}
