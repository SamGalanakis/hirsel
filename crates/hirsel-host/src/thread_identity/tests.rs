use super::*;

fn reference(id: u64, kind: ThreadKind, title: &str) -> ThreadIdentityRef {
    ThreadIdentityRef {
        id,
        kind,
        title: title.into(),
    }
}

fn identity(thread: ThreadIdentityRef) -> ThreadIdentity {
    ThreadIdentity {
        thread,
        description: String::new(),
        ancestors: vec![],
        reach: "self + subtree".into(),
    }
}

#[test]
fn top_level_space_states_its_own_name_and_omits_the_ancestor_line() {
    let mut block = identity(reference(2, ThreadKind::Space, "lash"));
    block.description = "Everything about the lash runtime.".into();
    assert_eq!(
        block.block(),
        "## Where you are\n\
         You are the agent of Space #2 \"lash\" (top level).\n\
         Description: Everything about the lash runtime.\n\
         Reach: self + subtree\n"
    );
}

#[test]
fn a_nested_task_lists_its_ancestors_root_first() {
    let mut block = identity(reference(9, ThreadKind::Task, "Ship the identity block"));
    block.description = "Name the Thread in the system prompt.".into();
    block.ancestors = vec![
        reference(1, ThreadKind::Space, "Hirsel"),
        reference(4, ThreadKind::Task, "Prompt work"),
    ];
    assert_eq!(
        block.block(),
        "## Where you are\n\
         You are the agent of Task #9 \"Ship the identity block\".\n\
         Description: Name the Thread in the system prompt.\n\
         Ancestors: Space #1 \"Hirsel\" › Task #4 \"Prompt work\"\n\
         Reach: self + subtree\n"
    );
}

#[test]
fn an_empty_description_says_so_rather_than_leaving_the_field_blank() {
    let block = identity(reference(3, ThreadKind::Task, "Untitled"));
    assert!(block.block().contains("Description: (none yet)\n"));
}

#[test]
fn a_multi_line_description_keeps_one_field_per_line() {
    let mut block = identity(reference(3, ThreadKind::Space, "Billing"));
    block.description = "Invoices.\nAnd dunning.".into();
    assert!(
        block
            .block()
            .contains("Description: Invoices.\n  And dunning.\nReach:")
    );
}

#[test]
fn a_grant_widens_the_reach_line() {
    let mut block = identity(reference(2, ThreadKind::Space, "lash"));
    block.reach = "self + subtree · +Space #7 \"Billing\"".into();
    assert!(
        block
            .block()
            .contains("Reach: self + subtree · +Space #7 \"Billing\"\n")
    );
}
