use super::{Refusal, caps, refusal};

#[test]
fn explicit_tool_refusal_is_distinct_from_worker_termination() {
    assert_eq!(
        refusal(
            "echo '\"ToolRefusal\"'; echo 'link tool refused' >&2; exit 2",
            caps()
        ),
        Refusal::ToolRefusal("link tool refused\n".into())
    );
}
