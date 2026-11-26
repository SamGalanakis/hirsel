//! The deploy and decide commands' refusals that need no host.
use super::{UNAVAILABLE, decide, deploy};

#[test]
fn contradictory_deploy_actions_refuse_before_io() {
    let recipe = "ab".repeat(32);
    let request = "01".repeat(16);
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("no-host");
    let invoke = |command: &str, extra: &[&str]| {
        let arguments = [command, "--state", state.to_str().unwrap()]
            .into_iter()
            .chain(extra.iter().copied())
            .map(str::to_owned);
        let crate::cli::Action::Invoke(invocation) = crate::cli::parse(arguments).unwrap() else {
            panic!("expected an operator invocation");
        };
        match command {
            "deploy" => deploy(invocation),
            "decide" => decide(invocation),
            _ => unreachable!(),
        }
    };
    for (command, args, want) in [
        (
            "deploy",
            &["--status", &request, "--approve-grant", "+Clock"][..],
            "deploy --status takes only --state",
        ),
        (
            "deploy",
            &["--status", &request, "--request-id", &request],
            "deploy --status takes only --state",
        ),
        (
            "deploy",
            &[&recipe, "--status", &request],
            "deploy --status takes only --state",
        ),
        (
            "deploy",
            &["--status", &request, "--held", "release"],
            "deploy --status takes only --state",
        ),
        (
            "decide",
            &[
                "--request-id",
                &request,
                "--refuse",
                "--approve-grant",
                "+Clock",
            ],
            "--refuse takes no --approve-grant, --held or --held-entry",
        ),
        (
            "decide",
            &["--request-id", &request, "--refuse", "--held", "release"],
            "--refuse takes no --approve-grant, --held or --held-entry",
        ),
        (
            "deploy",
            &[&recipe, "--held", "keep"],
            "--held takes release or cancel",
        ),
        (
            "deploy",
            &[&recipe, "--held", "release", "--held", "cancel"],
            "--held is given twice",
        ),
        (
            "deploy",
            &[
                &recipe,
                "--held-entry",
                "a=b=release",
                "--held-entry",
                "a=b=cancel",
            ],
            "--held-entry names a=b twice",
        ),
    ] {
        assert_eq!(invoke(command, args).unwrap_err().to_string(), want);
    }
    assert_eq!(
        invoke("deploy", &["--status", &request]).unwrap(),
        UNAVAILABLE
    );
    assert_eq!(
        invoke("decide", &["--request-id", &request, "--refuse"]).unwrap(),
        UNAVAILABLE
    );
    let error = invoke("deploy", &[&recipe, "--approve-grant", "+Clock"]).unwrap_err();
    assert_eq!(error.to_string(), "requires --code-store DIR");
}
