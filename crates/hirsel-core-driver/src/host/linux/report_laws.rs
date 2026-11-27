use std::fs::{self, File};
use std::path::Path;
use std::process::{Command, Stdio};

#[test]
fn json_test_report_refuses_a_full_output_file() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let driver = workspace.join(std::env::var_os("HIRSEL_DEV_TEST_DRIVER").unwrap());
    let runtime = workspace.join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").unwrap());
    let directory = tempfile::tempdir().unwrap();
    let app = directory.path();
    fs::write(
        app.join("hirsel.toml"),
        "[package]\nname = \"local/app\"\nversion = \"1.0.0\"\ncharge = \"charge-v1\"\n\n[sources]\n\"main.hirsel\" = \"main\"\n\n[tests]\nlimit = 67108864\n\n[tests.sources]\n\"test.hirsel\" = \"tests\"\n",
    )
    .unwrap();
    fs::write(
        app.join("main.hirsel"),
        "pub fn main() -> Result[(), Exit] { Ok(()) }\n",
    )
    .unwrap();
    fs::write(
        app.join("test.hirsel"),
        "@test\nfn good() -> Result[(), ()] { Ok(()) }\n",
    )
    .unwrap();
    let command = || {
        let mut command = Command::new(&driver);
        command
            .current_dir(app)
            .env("XDG_CACHE_HOME", app.join("cache"));
        command
    };
    let locked = command().args(["lock", "."]).output().unwrap();
    assert!(locked.status.success(), "{locked:?}");

    for stderr_full in [false, true] {
        let full = || Stdio::from(File::options().write(true).open("/dev/full").unwrap());
        let result = command()
            .args(["test", ".", "--json", "--libraries"])
            .arg(workspace.join("core"))
            .arg("--runtime")
            .arg(&runtime)
            .stdout(full())
            .stderr(if stderr_full { full() } else { Stdio::piped() })
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2), "{result:?}");
        if !stderr_full {
            let stderr = String::from_utf8(result.stderr).unwrap();
            assert!(
                stderr.contains("write test report: No space left on device"),
                "{stderr}"
            );
        }
    }
}
