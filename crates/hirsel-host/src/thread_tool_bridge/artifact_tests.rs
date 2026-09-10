use super::*;

fn artifact_result(response: Value) -> Value {
    assert_eq!(response["result"]["isError"], false, "{response}");
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn artifact_mcp_lifecycle_preserves_exact_whitespace_edits_and_replay() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let mut bridge = bridge(&state).await;
    let catalog = rpc(
        &bridge,
        "provider",
        "list-tools",
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await
    .unwrap();
    for name in [
        "artifacts_create",
        "artifacts_list",
        "artifacts_show",
        "artifacts_edit",
    ] {
        assert!(
            catalog["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == name)
        );
    }
    let created = artifact_result(
        rpc(
            &bridge,
            "provider",
            "create",
            call(
                "artifacts_create",
                json!({"title":"Document", "kind":"html", "content":"<h1>One</h1>\n<p>Two</p>"}),
            ),
        )
        .await
        .unwrap(),
    );
    let id = created["id"].as_u64().unwrap();
    let listed = artifact_result(
        rpc(
            &bridge,
            "provider",
            "list",
            call("artifacts_list", json!({})),
        )
        .await
        .unwrap(),
    );
    assert_eq!(listed["artifacts"][0]["id"], id);
    let shown = artifact_result(
        rpc(
            &bridge,
            "provider",
            "show",
            call("artifacts_show", json!({"artifact_id":id})),
        )
        .await
        .unwrap(),
    );
    assert_eq!(shown["content"], "<h1>One</h1>\n<p>Two</p>");
    let edit = call(
        "artifacts_edit",
        json!({"artifact_id":id,"edits":[{"old_string":"\n","new_string":"\n\n"}]}),
    );
    let edited_response = rpc(&bridge, "provider", "edit", edit.clone())
        .await
        .unwrap();
    let edited = artifact_result(edited_response.clone());
    assert_eq!(edited["content"], "<h1>One</h1>\n\n<p>Two</p>");
    assert_eq!(
        rpc(&bridge, "provider", "edit", edit).await.unwrap(),
        edited_response
    );
    assert_eq!(
        state.storage.artifact(id).await.unwrap().content,
        edited["content"].as_str().unwrap()
    );
    let messages = state
        .storage
        .thread_detail(bridge.caller.thread_id, None, 100)
        .await
        .unwrap()
        .messages;
    assert_eq!(messages.len(), 3);
    assert!(
        messages
            .iter()
            .all(|message| message.artifact_ids == vec![id])
    );
    let invalid = rpc(
        &bridge,
        "provider",
        "ambiguous",
        call(
            "artifacts_edit",
            json!({"artifact_id":id,"edits":[{"old_string":"\n","new_string":" "}]}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(invalid["result"]["isError"], true);
    assert_eq!(
        state.storage.artifact(id).await.unwrap().content,
        edited["content"].as_str().unwrap()
    );
    assert_eq!(
        state
            .storage
            .thread_detail(bridge.caller.thread_id, None, 100)
            .await
            .unwrap()
            .messages
            .len(),
        3
    );
    bridge.finish().await;
    let reopened = crate::storage::Storage::open(dir.path()).await.unwrap();
    assert_eq!(
        reopened.artifact(id).await.unwrap().content,
        "<h1>One</h1>\n\n<p>Two</p>"
    );
}
