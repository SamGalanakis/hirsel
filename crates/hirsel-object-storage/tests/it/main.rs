//! The object storage contract, run against every implementation.

hirsel_guard::install!(Counting);
use hirsel_object_storage::fake_s3::FakeS3;
use hirsel_object_storage::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The system allocator, counting the allocations each thread makes.
struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn count() {
    let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, block: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(block, layout, size) }
    }
    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        unsafe { System.dealloc(block, layout) }
    }
}

/// Allocations `f` makes on this thread.
fn allocations<T>(f: impl FnOnce() -> T) -> (u64, T) {
    let before = ALLOCATIONS.with(Cell::get);
    let out = f();
    (ALLOCATIONS.with(Cell::get) - before, out)
}

fn contract(objects: &dyn ObjectStorage) {
    assert_eq!(objects.get("code/a1").unwrap(), None);
    assert_eq!(objects.put_new("code/a1", b"first").unwrap(), Put::Created);
    assert_eq!(
        objects.get("code/a1").unwrap().as_deref(),
        Some(&b"first"[..])
    );
    // Create-if-absent never replaces, whatever the new bytes.
    assert_eq!(objects.put_new("code/a1", b"second").unwrap(), Put::Exists);
    assert_eq!(objects.put_new("code/a1", b"first").unwrap(), Put::Exists);
    assert_eq!(
        objects.get("code/a1").unwrap().as_deref(),
        Some(&b"first"[..])
    );
    assert_eq!(objects.put_new("sig/a1/k-2", b"").unwrap(), Put::Created);
    assert_eq!(
        objects.get("sig/a1/k-2").unwrap().as_deref(),
        Some(&b""[..])
    );
    // A replace takes effect exactly when its condition holds.
    assert_eq!(
        objects.replace("head", b"h1", Condition::Matches(Tag("x".into()))),
        Ok(Replaced::Mismatch)
    );
    let Ok(Replaced::Replaced(t1)) = objects.replace("head", b"h1", Condition::Absent) else {
        panic!("creating an absent head")
    };
    assert_eq!(
        objects.replace("head", b"h0", Condition::Absent),
        Ok(Replaced::Mismatch)
    );
    assert_eq!(
        objects.get_tagged("head").unwrap(),
        Some((b"h1".to_vec(), t1.clone()))
    );
    let Ok(Replaced::Replaced(t2)) = objects.replace("head", b"h2", Condition::Matches(t1.clone()))
    else {
        panic!("replacing on the current tag")
    };
    assert_ne!(t1, t2);
    // A stale tag no longer matches: the writer that holds it is fenced.
    assert_eq!(
        objects.replace("head", b"h3", Condition::Matches(t1)),
        Ok(Replaced::Mismatch)
    );
    assert_eq!(objects.get("head").unwrap().as_deref(), Some(&b"h2"[..]));
    // Listings are ordered, bounded by prefix and start after `after`;
    // nested keys are found, temporary files never are.
    for key in ["h/2", "h/1", "h/3/x", "hx"] {
        objects.put_new(key, b"").unwrap();
    }
    let list = |prefix: &str, after: &str| objects.list(prefix, after).unwrap().keys;
    assert_eq!(list("h/", ""), ["h/1", "h/2", "h/3/x"]);
    assert_eq!(list("h/", "h/1"), ["h/2", "h/3/x"]);
    assert_eq!(list("h", "h/3/x"), ["head", "hx"]);
    objects.delete("h/2").unwrap();
    objects.delete("h/2").unwrap();
    assert_eq!(objects.get("h/2").unwrap(), None);
    assert_eq!(list("h/", ""), ["h/1", "h/3/x"]);
    for bad in [
        "",
        "/x",
        "x/",
        "../x",
        "code/../x",
        "code/.tmp-1",
        "Code/x",
        "code//x",
        "a b",
    ] {
        assert_eq!(
            objects.get(bad),
            Err(StorageError::Key(bad.into())),
            "{bad:?}"
        );
        assert_eq!(
            objects.put_new(bad, b"x"),
            Err(StorageError::Key(bad.into())),
            "{bad:?}"
        );
    }
}

#[test]
fn a_directory_creates_each_object_once_and_keeps_it_across_reopening() {
    let dir = tempfile::tempdir().unwrap();
    contract(&DirObjects::open(dir.path().join("store")).unwrap());
    let reopened = DirObjects::open(dir.path().join("store")).unwrap();
    assert_eq!(
        reopened.get("code/a1").unwrap().as_deref(),
        Some(&b"first"[..])
    );
    // No temporary file outlives a put.
    let names: Vec<_> = std::fs::read_dir(dir.path().join("store/code"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["a1"]);
    // Pages hold at most LIST_PAGE keys and say when more follow.
    for i in 0..LIST_PAGE + 1 {
        reopened.put_new(&format!("p/{i:05}"), b"").unwrap();
    }
    let first = reopened.list("p/", "").unwrap();
    assert!(first.truncated && first.keys.len() == LIST_PAGE);
    let rest = reopened.list("p/", first.keys.last().unwrap()).unwrap();
    assert_eq!(
        (rest.keys, rest.truncated),
        (vec![format!("p/{LIST_PAGE:05}")], false)
    );
}

#[test]
fn memory_objects_follow_the_same_contract() {
    contract(&MemoryObjects::new());
}

#[test]
fn simulated_faults_are_seeded_and_a_lost_put_reply_may_have_created_the_object() {
    let run = |seed| {
        let objects = MemoryObjects::with_faults(seed, 3);
        let outcomes: Vec<_> = (0..24)
            .map(|i| objects.put_new(&format!("k{}", i % 6), b"v").is_ok())
            .collect();
        (outcomes, objects.contents())
    };
    assert_eq!(run(7), run(7));
    assert_ne!(run(7).0, run(8).0);
    // Some failed put still created its object: the failure was the reply.
    let objects = MemoryObjects::with_faults(7, 2);
    let mut created_by_failed_put = false;
    for i in 0..64 {
        let key = format!("k{i}");
        if objects.put_new(&key, b"v").is_err() && objects.contents().contains_key(&key) {
            created_by_failed_put = true;
        }
    }
    assert!(created_by_failed_put);
}

#[test]
fn entries_open_their_provider_and_refuse_unknown_members() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("objects");
    let entry = serde_json::json!({"provider": "objects.dir@1", "path": path});
    let objects: Arc<dyn ObjectStorage> = open(&entry).unwrap();
    objects.put_new("x", b"1").unwrap();
    assert!(path.join("x").exists());
    assert!(open(&serde_json::json!({"provider": "objects.sim@1", "seed": 3})).is_ok());
    for (entry, error) in [
        (
            serde_json::json!({"provider": "objects.dir@1", "path": "relative"}),
            "objects.dir@1 path is an absolute directory",
        ),
        (
            serde_json::json!({"provider": "objects.sim@1", "faults": 1}),
            "objects.sim@1 has no member \"faults\"",
        ),
        (
            serde_json::json!({"provider": "objects.s3@1"}),
            "objects.s3@1 needs endpoint",
        ),
        (
            serde_json::json!({"provider": "objects.s3@1", "cache_bytes": 1}),
            "objects.s3@1 has no member \"cache_bytes\"",
        ),
        (
            serde_json::json!({"provider": "objects.s4@1"}),
            "unsupported object storage provider objects.s4@1",
        ),
    ] {
        assert_eq!(open(&entry).err().as_deref(), Some(error));
    }
}

/// An `objects.s3@1` storage on `fake` under `prefix`.
fn s3(
    fake: &FakeS3,
    dir: &std::path::Path,
    prefix: &str,
    timeout_ms: u64,
) -> Result<Arc<dyn ObjectStorage>, String> {
    open(&fake.entry(dir, prefix, timeout_ms))
}

fn faults(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn an_s3_bucket_follows_the_same_contract_under_its_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeS3::start(1, BTreeMap::new());
    contract(&*s3(&fake, dir.path(), "tenant/store", 5000).unwrap());
    // Another prefix in the same bucket is another storage.
    let other = s3(&fake, dir.path(), "tenant/other", 5000).unwrap();
    assert_eq!(other.get("code/a1").unwrap(), None);
    assert_eq!(other.list("", "").unwrap().keys, Vec::<String>::new());
    let objects = fake.world.lock().unwrap().objects.clone();
    assert!(objects.contains_key("tenant/store/code/a1"));
    assert!(
        objects.keys().all(|k| k.starts_with("tenant/store/")),
        "{objects:?}"
    );
    // Pages hold at most LIST_PAGE keys and say when more follow.
    let store = s3(&fake, dir.path(), "", 5000).unwrap();
    for i in 0..LIST_PAGE + 1 {
        store.put_new(&format!("p/{i:05}"), b"").unwrap();
    }
    let first = store.list("p/", "").unwrap();
    assert!(first.truncated && first.keys.len() == LIST_PAGE);
    let rest = store.list("p/", first.keys.last().unwrap()).unwrap();
    assert_eq!(
        (rest.keys, rest.truncated),
        (vec![format!("p/{LIST_PAGE:05}")], false)
    );
}

/// Garage 2.4.1 answers 200 to a second create-only PUT and overwrites; a
/// server that ignores If-Match would let a fenced writer replace the head.
#[test]
fn a_server_that_ignores_conditional_writes_fails_the_open() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeS3::start(1, BTreeMap::new());
    fake.world.lock().unwrap().honour_if_none_match = false;
    let refused = s3(&fake, dir.path(), "", 5000).err().unwrap();
    assert!(
        refused.contains("does not honour If-None-Match: *"),
        "{refused}"
    );
    let fake = FakeS3::start(1, BTreeMap::new());
    fake.world.lock().unwrap().honour_if_match = false;
    let refused = s3(&fake, dir.path(), "", 5000).err().unwrap();
    assert!(refused.contains("does not honour If-Match"), "{refused}");
    // The probe leaves nothing behind, and a conforming server opens.
    let fake = FakeS3::start(1, BTreeMap::new());
    s3(&fake, dir.path(), "", 5000).unwrap();
    assert!(fake.world.lock().unwrap().objects.is_empty());
}

#[test]
fn credentials_never_appear_in_errors() {
    let dir = tempfile::tempdir().unwrap();
    let creds = dir.path().join("creds.json");
    std::fs::write(
        &creds,
        r#"{"access_key":"AKIDSECRETISH","secret_key":"hunter2-secret","bogus":"x"}"#,
    )
    .unwrap();
    let entry = serde_json::json!({
        "provider": "objects.s3@1", "endpoint": "http://127.0.0.1:1", "bucket": "bkt",
        "region": "us-east-1", "credentials": creds, "connect_timeout_ms": 200,
    });
    let e = open(&entry).err().unwrap();
    assert!(
        e.contains("no member \"bogus\"") && !e.contains("hunter2") && !e.contains("AKIDSECRETISH"),
        "{e}"
    );
    std::fs::write(
        &creds,
        r#"{"access_key":"AKIDSECRETISH","secret_key":"hunter2-secret"}"#,
    )
    .unwrap();
    // Port 1 refuses: the probe fails unavailable, naming no credential.
    let e = open(&entry).err().unwrap();
    assert!(
        e.contains("unavailable") && !e.contains("hunter2") && !e.contains("AKIDSECRETISH"),
        "{e}"
    );
}

/// A response lost after its effect, a 5xx after it, a truncated body or a
/// timeout is Unavailable, never a success or a Mismatch, and the write did
/// happen; the next request reconnects.
#[test]
fn uncertain_responses_are_unavailable_and_the_provider_reconnects() {
    let dir = tempfile::tempdir().unwrap();
    for fault in ["lost_response", "slow"] {
        let fake = FakeS3::start(3, BTreeMap::new());
        fake.world.lock().unwrap().slow_ms = 400;
        let store = s3(&fake, dir.path(), "", 150).unwrap();
        let Ok(Replaced::Replaced(t1)) = store.replace("head", b"h1", Condition::Absent) else {
            panic!("creating the head")
        };
        fake.world.lock().unwrap().faults = faults(&[(fault, 1)]);
        for i in 0..6 {
            let bytes = format!("h-{fault}-{i}");
            let r = store.replace("head", bytes.as_bytes(), Condition::Matches(t1.clone()));
            assert!(
                matches!(r, Err(StorageError::Unavailable(_))),
                "{fault} {i}: {r:?}"
            );
            let r = store.put_new(&format!("k{i}"), b"v");
            assert!(
                matches!(r, Err(StorageError::Unavailable(_))),
                "{fault} {i}: {r:?}"
            );
        }
        fake.world.lock().unwrap().faults.clear();
        std::thread::sleep(Duration::from_millis(450));
        let contents = fake.world.lock().unwrap().objects.clone();
        // The first lost replace applied; the later ones found another tag.
        assert_eq!(
            contents["head"],
            format!("h-{fault}-0").as_bytes(),
            "{fault}"
        );
        assert_eq!(contents.len(), 7, "{fault}");
        assert_eq!(
            store.get("k5").unwrap().as_deref(),
            Some(&b"v"[..]),
            "{fault}"
        );
    }
    // A refused request (503) had no effect and is Unavailable too.
    let fake = FakeS3::start(3, faults(&[]));
    let store = s3(&fake, dir.path(), "", 2000).unwrap();
    fake.world.lock().unwrap().faults = faults(&[("refuse", 1)]);
    assert!(matches!(
        store.put_new("x", b"1"),
        Err(StorageError::Unavailable(_))
    ));
    fake.world.lock().unwrap().faults.clear();
    assert_eq!(store.get("x").unwrap(), None);
    // A server closing every connection after one response is still served.
    fake.world.lock().unwrap().close_after_each = true;
    for i in 0..4 {
        assert_eq!(store.put_new(&format!("c{i}"), b"1").unwrap(), Put::Created);
    }
}

/// A wrong ETag in a replace's response makes the writer's next If-Match a
/// Mismatch, never an overwrite.
#[test]
fn a_wrong_tag_fences_rather_than_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeS3::start(3, BTreeMap::new());
    let store = s3(&fake, dir.path(), "", 2000).unwrap();
    fake.world.lock().unwrap().faults = faults(&[("wrong_etag", 1)]);
    let Ok(Replaced::Replaced(t)) = store.replace("head", b"h1", Condition::Absent) else {
        panic!("creating the head")
    };
    fake.world.lock().unwrap().faults.clear();
    assert_eq!(
        store.replace("head", b"h2", Condition::Matches(t)),
        Ok(Replaced::Mismatch)
    );
    assert_eq!(store.get("head").unwrap().as_deref(), Some(&b"h1"[..]));
}

/// get_many runs its GETs at once over several connections: eight slow
/// responses take about one delay, not eight.
#[test]
fn get_many_overlaps_its_requests() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeS3::start(3, BTreeMap::new());
    let store = s3(&fake, dir.path(), "", 5000).unwrap();
    let keys: Vec<String> = (0..8).map(|i| format!("g/{i}")).collect();
    for k in &keys[..7] {
        store.put_new(k, k.as_bytes()).unwrap();
    }
    {
        let mut w = fake.world.lock().unwrap();
        w.slow_ms = 300;
        w.faults = faults(&[("slow", 1)]);
    }
    let started = Instant::now();
    let got = store.get_many(&keys);
    let took = started.elapsed();
    for (k, g) in keys[..7].iter().zip(&got) {
        assert_eq!(g.as_ref().unwrap().as_deref(), Some(k.as_bytes()));
    }
    assert_eq!(got[7], Ok(None));
    assert!(took < Duration::from_millis(1200), "{took:?}");
}

/// Steady-state requests build their head, canonical request and signature
/// in reused buffers: a create-only PUT, a refused replace and a DELETE
/// allocate nothing; a GET allocates only the bytes and tag it returns.
#[test]
fn steady_state_requests_allocate_only_what_they_return() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeS3::start(3, BTreeMap::new());
    let store = s3(&fake, dir.path(), "zone", 5000).unwrap();
    let stale = Tag("\"stale\"".into());
    for i in 0..4 {
        store.put_new(&format!("w/{i}"), b"warm").unwrap();
        store.get_tagged("w/0").unwrap();
        store.delete(&format!("w/{i}")).unwrap();
        store
            .replace("head", b"x", Condition::Matches(stale.clone()))
            .unwrap();
    }
    for i in 0..16 {
        let key = format!("k/{i}");
        let (n, put) = allocations(|| store.put_new(&key, b"bytes"));
        assert_eq!((n, put), (0, Ok(Put::Created)), "put {i}");
        let (n, replaced) =
            allocations(|| store.replace("head", b"x", Condition::Matches(stale.clone())));
        // The clone of the condition's tag is the caller's.
        assert_eq!((n, replaced), (1, Ok(Replaced::Mismatch)), "replace {i}");
        let (n, got) = allocations(|| store.get_tagged(&key));
        assert_eq!(n, 2, "get {i}");
        assert_eq!(got.unwrap().unwrap().0, b"bytes");
        let (n, deleted) = allocations(|| store.delete(&key));
        assert_eq!((n, deleted), (0, Ok(())), "delete {i}");
    }
}

#[test]
fn request_cuts_are_taken_by_the_simulated_objects_and_refused_by_real_storage() {
    let dir = tempfile::tempdir().unwrap();
    let delays = serde_json::json!([{"at": 3, "lands": 4}]);
    for (cut, value) in CUTS.iter().map(|c| (*c, serde_json::json!(3))).chain([
        ("delays", delays),
        ("stale_read_at", serde_json::json!([3, 4])),
    ]) {
        let sim = serde_json::json!({"provider": "objects.sim@1", cut: value});
        assert!(open(&sim).is_ok(), "{cut}");
        for real in [
            serde_json::json!({"provider": "objects.dir@1", "path": dir.path(), cut: 3}),
            serde_json::json!({"provider": "objects.s3@1", cut: 3}),
        ] {
            let provider = real["provider"].as_str().unwrap().to_string();
            assert_eq!(
                open(&real).err(),
                Some(format!("{provider} has no member {cut:?}"))
            );
        }
    }
}

#[test]
fn a_cut_fires_at_its_request_and_a_crash_holds_until_restart() {
    let at = |cuts: Cuts| MemoryObjects::new().with_cuts(cuts);
    // Request 2 of each: a put after a get.
    let run = |o: &MemoryObjects| {
        let _ = o.get("x");
        let put = o.put_new("x", b"1");
        (put, o.contents().contains_key("x"))
    };
    let refused = at(Cuts {
        fail_at: Some(2),
        ..Cuts::default()
    });
    assert!(matches!(run(&refused), (Err(StorageError::Io(_)), false)));
    let dropped = at(Cuts {
        lose_unapplied_at: Some(2),
        ..Cuts::default()
    });
    assert!(matches!(
        run(&dropped),
        (Err(StorageError::Unavailable(_)), false)
    ));
    let lost = at(Cuts {
        lose_response_at: Some(2),
        ..Cuts::default()
    });
    assert!(matches!(
        run(&lost),
        (Err(StorageError::Unavailable(_)), true)
    ));
    let crashed = at(Cuts {
        crash_at: Some(2),
        ..Cuts::default()
    });
    assert!(matches!(run(&crashed), (Ok(Put::Created), true)));
    assert!(crashed.put_new("y", b"2").is_err());
    assert!(!crashed.contents().contains_key("y"));
    assert!(crashed.restart());
    assert!(!crashed.restart());
    assert_eq!(crashed.put_new("y", b"2"), Ok(Put::Created));
    // A held-back write is unanswered and absent until just before the
    // request it lands before, and its condition is checked when it lands.
    let held = at(Cuts {
        delays: vec![Delay { at: 2, lands: 4 }],
        ..Cuts::default()
    });
    assert!(matches!(
        run(&held),
        (Err(StorageError::Unavailable(_)), false)
    ));
    assert!(held.get("x").unwrap().is_none());
    assert_eq!(held.get("x").unwrap().as_deref(), Some(&b"1"[..]));
    // Request 2 replaces x if it still holds 0; request 3 deletes x first.
    let late = at(Cuts {
        delays: vec![Delay { at: 2, lands: 4 }],
        ..Cuts::default()
    });
    assert_eq!(late.put_new("x", b"0"), Ok(Put::Created));
    let zero = Condition::Matches(content_tag(b"0"));
    assert!(late.replace("x", b"1", zero).is_err());
    assert_eq!(late.delete("x"), Ok(()));
    assert_eq!(late.get("x").unwrap(), None);
    // A stale get answers the bytes the key's latest write replaced, at each
    // request listed; the other gets, and a key its latest write created,
    // answer as they are.
    let stale = |at: Vec<u64>| {
        let o = MemoryObjects::new().with_cuts(Cuts {
            stale_read_at: at,
            ..Cuts::default()
        });
        assert_eq!(o.put_new("x", b"0"), Ok(Put::Created));
        let zero = Condition::Matches(content_tag(b"0"));
        assert!(o.replace("x", b"1", zero).is_ok());
        [(); 3].map(|()| o.get("x").unwrap())
    };
    let (old, new) = (Some(b"0".to_vec()), Some(b"1".to_vec()));
    assert_eq!(stale(vec![3]), [old.clone(), new.clone(), new.clone()]);
    assert_eq!(stale(vec![4]), [new.clone(), old.clone(), new.clone()]);
    assert_eq!(stale(vec![3, 4]), [old.clone(), old.clone(), new.clone()]);
    for one in [
        serde_json::json!(3),
        serde_json::json!([]),
        serde_json::json!([0]),
    ] {
        let sim = serde_json::json!({"provider": "objects.sim@1", "stale_read_at": one});
        assert!(open(&sim).is_err(), "{one}");
    }
    let created = at(Cuts {
        stale_read_at: vec![2],
        ..Cuts::default()
    });
    assert_eq!(created.put_new("x", b"0"), Ok(Put::Created));
    assert_eq!(created.get("x").unwrap(), Some(b"0".to_vec()));
}

#[test]
fn clients_of_one_world_share_objects_count_and_script_and_crash_alone() {
    let sim = |world: &str, crash_at: u64| serde_json::json!({"provider": "objects.sim@1", "world": world, "crash_at": crash_at, "fail_at": 3});
    let client = |worlds: &Worlds, entry: &serde_json::Value| match provider(entry, Some(worlds)) {
        Ok(Provider::Sim(o)) => o,
        _ => panic!("a sim client"),
    };
    let worlds = Worlds::default();
    let a = client(&worlds, &sim("w", 2));
    let b = client(&worlds, &sim("w", 2));
    // One count: a sends request 1, b request 2 (and crashes after it), a
    // request 3 (refused).
    assert_eq!(a.put_new("x", b"1"), Ok(Put::Created));
    assert_eq!(b.get("x").unwrap().as_deref(), Some(&b"1"[..]));
    assert!(matches!(a.put_new("y", b"2"), Err(StorageError::Io(_))));
    // b alone is down; a is served, and b's restart serves b again.
    assert!(b.get("x").is_err());
    assert_eq!(a.put_new("y", b"2"), Ok(Put::Created));
    assert!(!a.restart());
    assert!(b.restart());
    assert_eq!(b.get("y").unwrap().as_deref(), Some(&b"2"[..]));
    // Another world, and an entry without one, are objects of their own.
    let other = client(&worlds, &sim("v", 2));
    let private = client(&worlds, &serde_json::json!({"provider": "objects.sim@1"}));
    for o in [&other, &private] {
        assert_eq!(o.get("x").unwrap(), None);
    }
    // A world is named by one entry, and only inside a run.
    assert!(provider(&sim("w", 5), Some(&worlds)).is_err());
    assert!(provider(&sim("u", 2), None).is_err());
}

#[test]
fn delayed_writes_stay_outstanding_together_and_land_in_script_order_across_a_restart() {
    // Request 1 puts x and request 3 deletes it, both held; request 2 crashes
    // the client, which restarts. The delete is listed first, so when both
    // land before request 5 the put lands last and x is there; listed the
    // other way round, x is gone. Neither lands before request 5, restart or
    // no restart.
    for (delays, after) in [
        (
            vec![Delay { at: 3, lands: 5 }, Delay { at: 1, lands: 5 }],
            Some(&b"1"[..]),
        ),
        (
            vec![Delay { at: 1, lands: 5 }, Delay { at: 3, lands: 5 }],
            None,
        ),
    ] {
        let o = MemoryObjects::new().with_cuts(Cuts {
            delays,
            crash_at: Some(2),
            ..Cuts::default()
        });
        assert!(o.put_new("x", b"1").is_err());
        assert!(o.get("y").unwrap().is_none());
        assert!(o.get("y").is_err());
        assert!(o.restart());
        assert!(o.delete("x").is_err());
        assert!(o.get("x").unwrap().is_none());
        assert_eq!(o.get("x").unwrap().as_deref(), after);
    }
}
