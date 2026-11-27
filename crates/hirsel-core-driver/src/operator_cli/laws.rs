// launch.md, "Source by content": --source sends a manifest of visible regular
// files and streams only the digests named by Need, in the host's order.
#[test]
fn source_walk_manifest_approvals_and_refusals() {
    use crate::launcher::session::Writer;
    use sha2::{Digest, Sha256};
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("no-host");
    let root = directory.path().join("src");
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::create_dir(root.join(".hidden")).unwrap();
    std::fs::write(root.join("z.txt"), b"z").unwrap();
    std::fs::write(root.join("a.txt"), b"a").unwrap();
    std::fs::write(root.join("nested/inner.hirsel"), b"inner").unwrap();
    std::fs::write(root.join(".hidden/file"), b"hidden").unwrap();
    std::fs::write(root.join("nested/.skip"), b"skip").unwrap();

    let invoke_source = |source: &std::path::Path, extra: &[&str]| -> Result<(), super::Error> {
        let mut lines = Vec::new();
        let mut code = None;
        for pair in extra.chunks_exact(2) {
            match pair[0] {
                "--code-store" => code = Some(pair[1].into()),
                "--grant" => lines.push(pair[1].to_owned()),
                _ => unreachable!(),
            }
        }
        super::submission(Some(source.into()), None, code, lines, &state).map(|_| ())
    };
    let grants = ["Clock", "Foreign=app@1/main/I", "Exec=tools.toml build"].map(str::to_owned);
    let (input, upload, source, approvals) =
        super::submission(Some(root.clone()), None, None, grants.to_vec(), &root).unwrap();
    assert!(upload.is_none());
    let crate::operator::Input::Source(manifest) = input else {
        panic!("--source did not submit a source manifest");
    };
    assert_eq!(approvals.foreign, ["app@1/main/I"]);
    assert_eq!(approvals.exec, ["tools.toml build"]);

    let files: [(&str, &[u8]); 3] = [
        ("a.txt", b"a"),
        ("nested/inner.hirsel", b"inner"),
        ("z.txt", b"z"),
    ];
    let digests = files.map(|(_, content)| <[u8; 32]>::from(Sha256::digest(content)));
    let mut expected = Writer::default();
    expected.bytes(b".");
    expected.strings(&grants);
    expected.len(0); // dirs
    expected.len(0); // required entries
    expected.len(files.len());
    for ((path, content), digest) in files.into_iter().zip(digests) {
        expected.bytes(path.as_bytes());
        expected.bytes(&digest);
        expected.u64(content.len() as u64);
    }
    assert_eq!(manifest, expected.0);
    let source = source.unwrap();
    assert_eq!(source.stream(&[digests[2], digests[0]]).unwrap(), b"za");
    assert_eq!(
        source.stream(&[[0u8; 32]]).unwrap_err().to_string(),
        format!(
            "the host asked for a blob {} the source does not hold",
            "00".repeat(32)
        )
    );

    for (args, expected) in [
        (
            &["--code-store", "code"][..],
            "--source sends the tree itself and takes no --code-store",
        ),
        (
            &["--grant", "Foreign=bad"],
            "expected Foreign=package@major/module/interface",
        ),
        (
            &[
                "--grant",
                "Foreign=app@1/main/I",
                "--grant",
                "Foreign=app@1/main/I",
            ],
            "a Foreign interface can be granted only once",
        ),
        (
            &["--grant", "Exec=tools.toml"],
            "Exec requires --grant 'Exec=FILE NAME[,NAME]'",
        ),
    ] {
        assert_eq!(
            invoke_source(&root, args).unwrap_err().to_string(),
            expected
        );
    }

    let missing = directory.path().join("missing");
    let not_found = std::io::Error::from_raw_os_error(libc::ENOENT);
    assert_eq!(
        invoke_source(&missing, &[]).unwrap_err().to_string(),
        format!("{}: {not_found}", missing.display())
    );
    let link = root.join("nested/link");
    symlink(directory.path(), &link).unwrap();
    assert_eq!(
        invoke_source(&root, &[]).unwrap_err().to_string(),
        "--source: nested/link is not a regular file or directory"
    );
    std::fs::remove_file(link).unwrap();
    let bad_name = root
        .join("nested")
        .join(std::ffi::OsString::from_vec(vec![0xff]));
    match std::fs::write(&bad_name, b"x") {
        Ok(()) => {
            assert_eq!(
                invoke_source(&root, &[]).unwrap_err().to_string(),
                "--source: � is not a UTF-8 name"
            );
            std::fs::remove_file(bad_name).unwrap();
        }
        // APFS refuses this filename before a source walk can encounter it.
        Err(error) => assert_eq!(error.raw_os_error(), Some(libc::EILSEQ)),
    }

    let large = directory.path().join("large");
    std::fs::create_dir(&large).unwrap();
    std::fs::File::create(large.join("a"))
        .unwrap()
        .set_len(4 << 20)
        .unwrap();
    std::fs::write(large.join("b"), b"b").unwrap();
    assert_eq!(
        invoke_source(&large, &[]).unwrap_err().to_string(),
        format!(
            "--source: {} holds 4194305 bytes of files, over the 4194304 a source may",
            large.display()
        )
    );
}

#[test]
fn slice1_resolve_identify_read() {
    slice1::resolve_identify_read();
}

#[path = "slice1_law.rs"]
mod slice1;
