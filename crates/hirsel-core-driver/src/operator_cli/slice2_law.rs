//! The operator grammar used by automation and by the generated command inventory.
#[test]
fn submit_and_decide_commands() {
    let operation = format!("host/{:032x}/{:032x}", 1, 2);
    let recipe = "ab".repeat(32);
    let accepts =
        |args: &[&str]| crate::cli::parse(args.iter().map(|word| word.to_string())).is_ok();
    for args in [
        vec![
            "deploy",
            "--recipe",
            &recipe,
            "--create",
            "group.hcfg",
            "--plan",
        ],
        vec!["deploy", "member", "--source", "root", "--wait=5s"],
        vec!["op", "decide", &operation, "--decision", "-"],
        vec!["op", "get", &operation, "--wait"],
    ] {
        assert!(accepts(&args), "{args:?}");
    }
    for args in [
        vec!["register"],
        vec!["decide"],
        vec!["deploy", "--status", &operation],
        vec!["deploy", "--approve-grant", "+Clock"],
        vec!["deploy", "--held", "cancel"],
        vec!["deploy", "--held-entry", "entry=release"],
        vec!["deploy", "--approve-display", &recipe],
        vec!["deploy", "--approve-migration", &recipe],
    ] {
        assert!(!accepts(&args), "{args:?}");
    }
}

#[test]
fn terminal_and_document_choose_identical_wire_bytes() {
    use super::{config, decision};
    use crate::{
        launcher::session::{Grants, Wire},
        operator::*,
    };
    use std::{fs::File, io::Write};
    let operation = vec![0x42; 16];
    let plan = DeployPlan {
        domain: "hirsel.host.deploy.plan.v1".into(),
        authority: CatalogStore { hi: 1, lo: 2 },
        request: operation.clone(),
        intent_digest: vec![3; 32],
        input_digest: vec![4; 32],
        create: None,
        entries: Some(vec![]),
        incarnation: Some(7),
        subject: Precondition {
            name: "orders".into(),
            generation: 7,
            predecessor: Some(1),
            assembly: vec![],
            store_id: None,
            policy_digest: vec![],
        },
        facts: Decision {
            gate_toolchain: None,
            tree: None,
            root: None,
            stage: DecisionStage::Prepared,
            grants: Grants::default(),
            approvals: None,
            migration: None,
            triggers: TriggerReport {
                unmatched: vec![],
                rows: vec![TriggerLine {
                    class: 1,
                    kind: 1,
                    path: b"orders/send".to_vec(),
                    schema: vec![5; 32],
                    hold: Some(HeldBy::Cut),
                    fate: None,
                    count: 1,
                    earliest_due: 1,
                }],
            },
        },
    };
    let hash = [0x56; 32];
    let (master, slave) = crate::terminal_fixture::pty();
    let (mut master, mut terminal) = (File::from(master), File::from(slave));
    master.write_all(b"approve\neach\ncancel\n").unwrap();
    let mut reader = std::io::BufReader::new(terminal.try_clone().unwrap());
    let entered = decision::interactive(&plan, &hash, &mut reader, &mut terminal)
        .ok()
        .unwrap();
    let file = format!(
        "host_v1.Decision(plan_digest: \"{}\", verdict: Approve, entries: [(\"orders/send\", Cancel)])",
        "56".repeat(32)
    );
    let document = config::decode(file.as_bytes()).ok().unwrap();
    let frame = |doc| {
        decision::frame(&operation, &plan, &hash, doc)
            .ok()
            .unwrap()
            .encode()
    };
    let expected = Ask::Decide(
        operation.clone(),
        hash.to_vec(),
        Verdict::Approve(Fates {
            all: None,
            entries: vec![(b"orders/send".to_vec(), HeldFate::Cancel)],
        }),
    )
    .encode();
    assert_eq!(frame(entered), expected);
    assert_eq!(frame(document), expected);
    for entries in [
        "[]",
        "[(\"other\", Cancel)]",
        "[(\"orders/send\", Cancel),(\"extra\", Release)]",
        "[(\"orders/send\", Cancel),(\"orders/send\", Release)]",
    ] {
        let data = file.replace("[(\"orders/send\", Cancel)]", entries);
        assert!(
            config::decode(data.as_bytes())
                .and_then(|d| decision::frame(&operation, &plan, &hash, d))
                .is_err(),
            "{entries}"
        );
    }
}
