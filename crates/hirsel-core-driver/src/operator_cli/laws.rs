//! The deploy command's refusals that need no host.
use super::{UNAVAILABLE, deploy};

#[test]
fn contradictory_deploy_actions_refuse_before_io() {
    let recipe = "ab".repeat(32);
    let request = "01".repeat(16);
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("no-host");
    let invoke = |extra: &[&str]| {
        let arguments = ["deploy", "--state", state.to_str().unwrap()]
            .into_iter()
            .chain(extra.iter().copied())
            .map(str::to_owned);
        let crate::cli::Action::Invoke(invocation) = crate::cli::parse(arguments).unwrap() else {
            panic!("expected a deploy invocation");
        };
        deploy(invocation)
    };
    for (args, want) in [
        (
            &["--status", &request, "--refuse-grants"][..],
            "deploy --status takes only --state",
        ),
        (
            &["--status", &request, "--approve-grant", "+Clock"],
            "deploy --status takes only --state",
        ),
        (
            &[&recipe, "--refuse-grants"],
            "deploy with a recipe takes no --refuse-grants",
        ),
        (
            &[&recipe, "--refuse-grants", "--approve-grant", "+Clock"],
            "--refuse-grants takes no --approve-grant",
        ),
        (
            &["--status", &request, "--held", "release"],
            "deploy --status takes only --state",
        ),
        (
            &[
                "--request-id",
                &request,
                "--refuse-grants",
                "--held-entry",
                "p=cancel",
            ],
            "deploy decisions take only --state and --request-id",
        ),
        (
            &[&recipe, "--held", "keep"],
            "--held takes release or cancel",
        ),
        (
            &[&recipe, "--held", "release", "--held", "cancel"],
            "--held is given twice",
        ),
        (
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
        assert_eq!(invoke(args).unwrap_err().to_string(), want);
    }
    assert_eq!(invoke(&["--status", &request]).unwrap(), UNAVAILABLE);
    assert_eq!(
        invoke(&["--request-id", &request, "--refuse-grants"]).unwrap(),
        UNAVAILABLE
    );
    let error = invoke(&[&recipe, "--approve-grant", "+Clock"]).unwrap_err();
    assert_eq!(error.to_string(), "requires --code-store DIR");
}
