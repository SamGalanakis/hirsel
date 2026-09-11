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
    let source = "🦀".repeat(20_000);
    std::fs::write(directory.path().join("large.txt"), source).expect("fixture");
    let tools = NativeCodingTools::new(directory.path().to_path_buf()).expect("tools");

    let result = outcome_value(call(&tools, "read", &json!({ "path": "large.txt" })).await);
    let content = result["content"].as_str().expect("text content");
    assert!(content.len() <= 50 * 1024);
    assert!(std::str::from_utf8(content.as_bytes()).is_ok());
    assert_eq!(result["line_truncated"], true);
    assert_eq!(result["truncated"], true);
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
async fn shell_uses_accepted_cwd_and_reports_nonzero_exit_as_data() {
    let directory = tempfile::tempdir().expect("tempdir");
    let tools = Arc::new(NativeCodingTools::new(directory.path().to_path_buf()).expect("tools"));
    let result = coordinated_shell_call(
        tools,
        json!({ "cmd": "printf '%s' \"$PWD\"; exit 7", "timeout_ms": 5000 }),
    )
    .await
    .value_for_projection();
    assert_eq!(result["exit_code"], 7);
    assert_eq!(result["output"], directory.path().display().to_string());
}

#[tokio::test]
async fn shell_timeout_is_a_bounded_failure() {
    let directory = tempfile::tempdir().expect("tempdir");
    let tools = Arc::new(NativeCodingTools::new(directory.path().to_path_buf()).expect("tools"));
    let output =
        coordinated_shell_call(tools, json!({ "cmd": "sleep 30", "timeout_ms": 20 })).await;
    assert!(matches!(output.outcome, ToolCallOutcome::Failure(_)));
    let result = output.value_for_projection();
    assert_eq!(result["code"], "shell_timeout");
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
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "owned child {pid} survived shutdown");

    let rejected = outcome_output(call(&tools, "read", &json!({ "path": "child.pid" })).await);
    assert!(matches!(rejected.outcome, ToolCallOutcome::Cancelled(_)));
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
