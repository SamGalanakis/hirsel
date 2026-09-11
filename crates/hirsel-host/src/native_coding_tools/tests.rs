use std::{sync::Arc, time::Duration};

use lash::tools::{ToolCall, ToolOutcome, ToolProvider};
use lash_core::{ToolCallOutcome, ToolCallOutput, ToolValue};
use serde_json::{Value, json};

use super::NativeCodingTools;

fn outcome_value(outcome: ToolOutcome) -> Value {
    match outcome {
        ToolOutcome::Done(output) => output.value_for_projection(),
        ToolOutcome::Pending(_) => panic!("native coding tools must not defer"),
    }
}

fn outcome_output(outcome: ToolOutcome) -> ToolCallOutput {
    match outcome {
        ToolOutcome::Done(output) => *output,
        ToolOutcome::Pending(_) => panic!("native coding tools must not defer"),
    }
}

async fn call(tools: &NativeCodingTools, name: &str, args: &Value) -> ToolOutcome {
    let context = lash_core::testing::mock_attempt_context();
    tools
        .execute(ToolCall {
            name,
            args,
            context: &context,
        })
        .await
}

async fn coordinated_shell_call(tools: Arc<NativeCodingTools>, args: Value) -> ToolCallOutput {
    let prepared = lash_core::PreparedToolCall::from_parts(
        "call-1",
        "hirsel:native-coding:exec-command:v1",
        "exec_command",
        args,
        None,
        Value::Null,
    );
    let effect_controller = lash_core::ScopedEffectController::shared(
        Arc::new(
            lash::runtime::NativeRuntimeEffectController::default()
                .allow_process_lifetime_completion_keys(),
        ),
        lash_core::ExecutionScope::runtime_operation("native-tools-shell-test"),
    )
    .expect("effect controller");
    let provider: Arc<dyn ToolProvider> = tools;
    lash_core::testing::coordinate_tool_provider_with_services(
        effect_controller,
        Arc::new(lash_core::testing::MockSessionManager::default()),
        "native-tools-test-session",
        super::exec_definition(),
        provider,
        prepared,
    )
    .await
    .expect("coordinated shell call")
    .output
}

#[test]
fn catalog_is_exactly_the_four_tool_profile() {
    let directory = tempfile::tempdir().expect("tempdir");
    let tools = NativeCodingTools::new(directory.path().to_path_buf()).expect("tools");
    let names = tools
        .tool_manifests()
        .into_iter()
        .map(|manifest| manifest.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["read", "edit", "write", "exec_command"]);
}

#[tokio::test]
async fn read_returns_unicode_safe_bounded_line_windows() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        directory.path().join("unicode.txt"),
        "zero\nαβγ\nthird\nfourth\n",
    )
    .expect("fixture");
    let tools = NativeCodingTools::new(directory.path().to_path_buf()).expect("tools");

    let result = outcome_value(
        call(
            &tools,
            "read",
            &json!({ "path": "unicode.txt", "offset": 2, "limit": 2 }),
        )
        .await,
    );
    assert_eq!(result["content"], "αβγ\nthird\n");
    assert_eq!(result["start_line"], 2);
    assert_eq!(result["end_line"], 3);
    assert_eq!(result["next_offset"], 4);
    assert_eq!(result["truncated"], true);
}

#[tokio::test]
async fn read_truncates_a_large_unicode_line_at_a_character_boundary() {
    let directory = tempfile::tempdir().expect("tempdir");
    let source = format!("{}\nsecond line\n", "🦀".repeat(20_000));
    std::fs::write(directory.path().join("large.txt"), &source).expect("fixture");
    let tools = NativeCodingTools::new(directory.path().to_path_buf()).expect("tools");

    let mut reconstructed = String::new();
    let mut offset = 1_u64;
    let mut byte_offset = 0_u64;
    for call_index in 0..10 {
        let result = outcome_value(
            call(
                &tools,
                "read",
                &json!({
                    "path": "large.txt",
                    "offset": offset,
                    "byte_offset": byte_offset
                }),
            )
            .await,
        );
        let content = result["content"].as_str().expect("text content");
        assert!(content.len() <= 50 * 1024);
        assert!(std::str::from_utf8(content.as_bytes()).is_ok());
        reconstructed.push_str(content);
        if call_index == 0 {
            assert_eq!(result["line_truncated"], true);
            assert_eq!(result["next_offset"], 1);
            assert!(result["next_byte_offset"].as_u64().unwrap() > 0);
        }
        if result["truncated"] == false {
            break;
        }
        offset = result["next_offset"].as_u64().expect("next line");
        byte_offset = result["next_byte_offset"].as_u64().expect("next byte");
    }
    assert_eq!(reconstructed, source);
}

#[tokio::test]
async fn read_returns_supported_images_as_typed_attachments() {
    let directory = tempfile::tempdir().expect("tempdir");
    image::GrayImage::new(1, 1)
        .save(directory.path().join("sample.png"))
        .expect("fixture");
    let tools = NativeCodingTools::new(directory.path().to_path_buf()).expect("tools");

    let output = outcome_output(call(&tools, "read", &json!({ "path": "sample.png" })).await);
    assert_eq!(output.attachments().len(), 1);
    let ToolCallOutcome::Success(ToolValue::Array(parts)) = output.outcome else {
        panic!("expected image success")
    };
    assert!(matches!(
        parts.as_slice(),
        [ToolValue::String(_), ToolValue::Attachment(_)]
    ));
}

#[tokio::test]
async fn edit_requires_one_exact_match_and_never_partially_changes_on_error() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("edit.txt");
    std::fs::write(&path, "same\nmiddle\nsame\n").expect("fixture");
    let tools = NativeCodingTools::new(directory.path().to_path_buf()).expect("tools");

    let ambiguous = outcome_output(
        call(
            &tools,
            "edit",
            &json!({ "path": "edit.txt", "old_text": "same", "new_text": "changed" }),
        )
        .await,
    );
    assert!(matches!(ambiguous.outcome, ToolCallOutcome::Failure(_)));
    assert_eq!(
        std::fs::read_to_string(&path).expect("unchanged"),
        "same\nmiddle\nsame\n"
    );

    let missing = outcome_output(
        call(
            &tools,
            "edit",
            &json!({ "path": "edit.txt", "old_text": "absent", "new_text": "changed" }),
        )
        .await,
    );
    assert!(matches!(missing.outcome, ToolCallOutcome::Failure(_)));
    assert_eq!(
        std::fs::read_to_string(&path).expect("unchanged"),
        "same\nmiddle\nsame\n"
    );

    std::fs::write(&path, "aaa").expect("overlap fixture");
    let overlapping = outcome_output(
        call(
            &tools,
            "edit",
            &json!({ "path": "edit.txt", "old_text": "aa", "new_text": "b" }),
        )
        .await,
    );
    assert!(matches!(overlapping.outcome, ToolCallOutcome::Failure(_)));
    assert_eq!(std::fs::read_to_string(&path).expect("unchanged"), "aaa");

    std::fs::write(&path, "same\nmiddle\nsame\n").expect("restore fixture");

    let unique = outcome_value(
        call(
            &tools,
            "edit",
            &json!({ "path": "edit.txt", "old_text": "middle", "new_text": "changed" }),
        )
        .await,
    );
    assert_eq!(unique["replacements"], 1);
    assert_eq!(
        std::fs::read_to_string(path).expect("edited"),
        "same\nchanged\nsame\n"
    );
}

#[tokio::test]
async fn write_creates_parents_and_atomically_replaces_complete_utf8() {
    let directory = tempfile::tempdir().expect("tempdir");
    let tools = NativeCodingTools::new(directory.path().to_path_buf()).expect("tools");
    let path = directory.path().join("nested/file.txt");

    call(
        &tools,
        "write",
        &json!({ "path": "nested/file.txt", "content": "first" }),
    )
    .await;
    let result = outcome_value(
        call(
            &tools,
            "write",
            &json!({ "path": "nested/file.txt", "content": "δεύτερο" }),
        )
        .await,
    );
    assert_eq!(result["bytes_written"], "δεύτερο".len());
    assert_eq!(std::fs::read_to_string(path).expect("written"), "δεύτερο");
    assert_eq!(
        std::fs::read_dir(directory.path().join("nested"))
            .expect("read dir")
            .count(),
        1,
        "temporary file must not remain after publication"
    );
}

#[tokio::test]
async fn shell_uses_explicit_posix_profile_cwd_and_nonzero_result_data() {
    let directory = tempfile::tempdir().expect("tempdir");
    let tools = Arc::new(NativeCodingTools::new(directory.path().to_path_buf()).expect("tools"));
    let result = coordinated_shell_call(
        tools,
        json!({ "cmd": "printf '%s|%s' \"$PWD\" \"$0\"; exit 7", "timeout_ms": 5000 }),
    )
    .await
    .value_for_projection();
    assert_eq!(result["exit_code"], 7);
    assert_eq!(
        result["output"],
        format!("{}|/bin/sh", directory.path().display())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn shell_normal_and_nonzero_exits_reap_background_descendants() {
    for status in [0, 7] {
        let directory = tempfile::tempdir().expect("tempdir");
        let tools =
            Arc::new(NativeCodingTools::new(directory.path().to_path_buf()).expect("tools"));
        let result = coordinated_shell_call(
            tools.clone(),
            json!({
                "cmd": format!(
                    "sleep 30 >/dev/null 2>&1 & echo $! > descendant.pid; exit {status}"
                ),
                "timeout_ms": 5000
            }),
        )
        .await
        .value_for_projection();
        assert_eq!(result["exit_code"], status);

        tools.shutdown().await;
        let pid = std::fs::read_to_string(directory.path().join("descendant.pid"))
            .expect("pid")
            .trim()
            .parse::<i32>()
            .expect("numeric pid");
        assert!(
            wait_for_process_exit(pid).await,
            "background descendant {pid} survived status {status}"
        );
    }
}

#[tokio::test]
async fn shell_timeout_is_a_bounded_failure() {
    let directory = tempfile::tempdir().expect("tempdir");
    let tools = Arc::new(NativeCodingTools::new(directory.path().to_path_buf()).expect("tools"));
    let output = coordinated_shell_call(
        tools.clone(),
        json!({
            "cmd": "printf '%s' \"$$\" > timeout.pid; exec sleep 30",
            "timeout_ms": 200
        }),
    )
    .await;
    assert!(matches!(output.outcome, ToolCallOutcome::Failure(_)));
    let result = output.value_for_projection();
    assert_eq!(result["code"], "shell_timeout");
    let pid = std::fs::read_to_string(directory.path().join("timeout.pid"))
        .expect("pid")
        .trim()
        .parse::<i32>()
        .expect("numeric pid");
    assert!(
        wait_for_process_exit(pid).await,
        "timed-out shell {pid} survived return"
    );
    tools.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_cancels_and_reaps_an_owned_shell_process() {
    let directory = tempfile::tempdir().expect("tempdir");
    let tools = Arc::new(NativeCodingTools::new(directory.path().to_path_buf()).expect("tools"));
    let prepared = lash_core::PreparedToolCall::from_parts(
        "call-1",
        "hirsel:native-coding:exec-command:v1",
        "exec_command",
        json!({ "cmd": "echo $$ > child.pid; exec sleep 30", "timeout_ms": 30000 }),
        None,
        Value::Null,
    );
    let effect_controller = lash_core::ScopedEffectController::shared(
        Arc::new(
            lash::runtime::NativeRuntimeEffectController::default()
                .allow_process_lifetime_completion_keys(),
        ),
        lash_core::ExecutionScope::runtime_operation("native-tools-shutdown-test"),
    )
    .expect("effect controller");
    let provider: Arc<dyn ToolProvider> = tools.clone();
    let running = lash_core::testing::coordinate_tool_provider_with_services(
        effect_controller,
        Arc::new(lash_core::testing::MockSessionManager::default()),
        "native-tools-test-session",
        super::exec_definition(),
        provider,
        prepared,
    );
    let shutdown = async {
        let pid_path = directory.path().join("child.pid");
        for _ in 0..100 {
            if pid_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(pid_path.exists(), "command did not publish its pid");
        let ((), ()) = tokio::join!(tools.shutdown(), tools.shutdown());
    };

    let (outcome, ()) = tokio::join!(running, shutdown);
    let completed = outcome.expect("coordinated tool call");
    assert!(matches!(
        completed.output.outcome,
        ToolCallOutcome::Cancelled(_)
    ));
    let pid = std::fs::read_to_string(directory.path().join("child.pid"))
        .expect("pid")
        .trim()
        .parse::<i32>()
        .expect("numeric pid");
    assert!(
        wait_for_process_exit(pid).await,
        "owned child {pid} survived shutdown"
    );

    let rejected = outcome_output(call(&tools, "read", &json!({ "path": "child.pid" })).await);
    assert!(matches!(rejected.outcome, ToolCallOutcome::Cancelled(_)));
}

#[cfg(unix)]
#[tokio::test]
async fn abandoned_result_consumer_does_not_abandon_early_shell_termination() {
    let directory = tempfile::tempdir().expect("tempdir");
    let tools = Arc::new(NativeCodingTools::new(directory.path().to_path_buf()).expect("tools"));
    let running = tokio::spawn(coordinated_shell_call(
        tools.clone(),
        json!({
            "cmd": "printf '%s' \"$$\" > early.pid; sleep 1; kill -KILL $$",
            "timeout_ms": 30000
        }),
    ));
    let pid_path = directory.path().join("early.pid");
    for _ in 0..200 {
        if pid_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(pid_path.exists(), "command did not publish its pid");
    let pid = std::fs::read_to_string(&pid_path)
        .expect("pid")
        .trim()
        .parse::<i32>()
        .expect("numeric pid");

    running.abort();
    assert!(
        running
            .await
            .expect_err("result consumer should abort")
            .is_cancelled()
    );
    for _ in 0..300 {
        if !process_exists(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !process_exists(pid),
        "shell did not terminate before its wrapper status publication"
    );
    tokio::time::timeout(Duration::from_secs(5), tools.shutdown())
        .await
        .expect("shutdown must join the retained terminal shell task");
}

#[cfg(unix)]
#[tokio::test]
async fn attempt_cancellation_reaps_owned_shell_before_returning() {
    let directory = tempfile::tempdir().expect("tempdir");
    let tools = Arc::new(NativeCodingTools::new(directory.path().to_path_buf()).expect("tools"));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let call_tools = tools.clone();
    let call_cancellation = cancellation.clone();
    let running = tokio::spawn(async move {
        call_tools
            .execute_shell(
                &json!({
                    "cmd": "printf '%s' \"$$\" > cancelled.pid; exec sleep 30",
                    "timeout_ms": 30000
                }),
                call_cancellation,
            )
            .await
    });
    let pid_path = directory.path().join("cancelled.pid");
    for _ in 0..200 {
        if pid_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(pid_path.exists(), "command did not publish its pid");
    let pid = std::fs::read_to_string(&pid_path)
        .expect("pid")
        .trim()
        .parse::<i32>()
        .expect("numeric pid");

    cancellation.cancel();
    let output = outcome_output(
        tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .expect("attempt cancellation must complete")
            .expect("caller task"),
    );
    assert!(matches!(output.outcome, ToolCallOutcome::Cancelled(_)));
    assert!(
        wait_for_process_exit(pid).await,
        "cancelled shell {pid} survived return"
    );
    tools.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn abandoned_file_mutation_is_drained_before_shutdown_returns() {
    use std::{ffi::CString, io::Write, os::unix::ffi::OsStrExt};

    let directory = tempfile::tempdir().expect("tempdir");
    let fifo = directory.path().join("blocked-edit");
    let fifo_name = CString::new(fifo.as_os_str().as_bytes()).expect("fifo path");
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
    let tools = Arc::new(NativeCodingTools::new(directory.path().to_path_buf()).expect("tools"));

    let edit_tools = tools.clone();
    let edit = tokio::spawn(async move {
        call(
            &edit_tools,
            "edit",
            &json!({ "path": "blocked-edit", "old_text": "x", "new_text": "y" }),
        )
        .await
    });
    let writer_path = fifo.clone();
    let mut writer = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .write(true)
                .open(writer_path)
                .expect("open fifo writer")
        }),
    )
    .await
    .expect("edit must open fifo reader")
    .expect("writer task");

    edit.abort();
    assert!(
        edit.await
            .expect_err("result consumer should abort")
            .is_cancelled()
    );
    let shutdown_tools = tools.clone();
    let mut shutdown = tokio::spawn(async move { shutdown_tools.shutdown().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut shutdown)
            .await
            .is_err(),
        "shutdown returned while the owned edit was blocked"
    );
    let rejected = outcome_output(
        call(
            &tools,
            "write",
            &json!({
                "path": "late.txt",
                "content": "must not publish"
            }),
        )
        .await,
    );
    assert!(matches!(rejected.outcome, ToolCallOutcome::Cancelled(_)));
    writer.write_all(b"x").expect("write fifo");
    drop(writer);
    shutdown.await.expect("shutdown task");
    assert_eq!(std::fs::read_to_string(&fifo).expect("edited"), "y");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        std::fs::read_to_string(&fifo).expect("stable after shutdown"),
        "y",
        "file mutation changed after shutdown returned"
    );
    assert!(
        !directory.path().join("late.txt").exists(),
        "shutdown admitted a late file write"
    );
}

#[tokio::test]
async fn shell_large_output_has_a_readable_full_output_path() {
    let directory = tempfile::tempdir().expect("tempdir");
    let tools = Arc::new(NativeCodingTools::new(directory.path().to_path_buf()).expect("tools"));
    let result = coordinated_shell_call(
        tools,
        json!({
            "cmd": "yes output | head -c 60000",
            "timeout_ms": 5000,
            "max_output_tokens": 64
        }),
    )
    .await
    .value_for_projection();
    assert!(result["original_token_count"].as_u64().is_some());
    let spill = result["full_output_path"]
        .as_str()
        .expect("large output spill path");
    assert!(
        std::fs::metadata(spill).expect("readable spill").len() >= 60_000,
        "spill must retain complete raw output"
    );
    std::fs::remove_file(spill).expect("remove owned test spill");
}

#[test]
fn constructor_rejects_a_non_directory_cwd() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("file");
    std::fs::write(&path, "x").expect("fixture");
    assert!(NativeCodingTools::new(path).is_err());
}

#[cfg(unix)]
fn process_exists(pid: i32) -> bool {
    (unsafe { libc::kill(pid, 0) }) == 0
}

#[cfg(unix)]
async fn wait_for_process_exit(pid: i32) -> bool {
    for _ in 0..500 {
        if !process_exists(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}
