use std::path::PathBuf;

use color_eyre::eyre::ensure;

use super::*;

#[test]
fn failed_process_spawn_removes_owned_logs_and_preserves_the_error() {
    let mut created = PathBuf::new();
    let result = with_temporary_log_root(|root| {
        created = root.to_path_buf();
        let mut process = Command::new(root.join("missing-program"));
        process
            .stdout(File::create(root.join("stdout"))?)
            .stderr(File::create(root.join("stderr"))?);
        let mut child = kraai_sandbox::spawn_command(&mut process)?;
        child.wait()?;
        Ok(())
    });
    let error = result.expect_err("missing command unexpectedly started");
    assert!(
        error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(!created.as_os_str().is_empty() && !created.exists());
}

#[test]
fn log_cleanup_preserves_primary_errors_and_reports_final_cleanup_errors() {
    let primary: Result<()> = with_temporary_log_root(|root| {
        fs::remove_dir(root)?;
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "primary failure").into())
    });
    let error = primary.expect_err("primary failure was lost");
    assert!(error.to_string() == "primary failure");

    let cleanup = with_temporary_log_root(|root| {
        fs::remove_dir(root)?;
        Ok(())
    });
    let error = cleanup.expect_err("final cleanup failure was lost");
    assert!(
        error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    );
}

#[test]
fn clean_environment_does_not_inherit_controller_values() {
    let env_program = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join("env"))
                .find(|candidate| candidate.is_file())
        })
        .expect("test requires env");
    let command = vec![env_program.to_string_lossy().into_owned()];

    let outcome = run_trusted_with_clean_environment(
        &command,
        Path::new("/"),
        kraai_test_support::HANG_TIMEOUT,
        &BTreeMap::new(),
    )
    .unwrap();

    assert!(outcome.success(), "environment command failed");
    assert!(
        outcome.stdout.is_empty(),
        "clean command inherited environment"
    );
    assert!(
        outcome.stderr.is_empty(),
        "environment command wrote stderr"
    );
}

const SPARSE_BYTES: u64 = 1 << 40;

#[test]
fn captured_output_reads_share_one_limit_regardless_of_sparse_file_size() {
    with_temporary_log_root(|root| {
        let stdout = root.join("stdout");
        let stderr = root.join("stderr");
        fs::write(&stdout, b"out")?;
        fs::write(&stderr, b"err")?;
        ensure!(
            read_captured_output(&stdout, &stderr, 6)? == (b"out".to_vec(), b"err".to_vec(), false)
        );
        ensure!(
            read_captured_output(&stdout, &stderr, 5)? == (b"out".to_vec(), b"er".to_vec(), true)
        );

        File::options()
            .write(true)
            .open(&stderr)?
            .set_len(SPARSE_BYTES)?;
        let (out, err, truncated) = read_captured_output(&stdout, &stderr, 1024)?;
        ensure!(out == b"out" && err.len() == 1021 && truncated);

        File::options()
            .write(true)
            .open(&stdout)?
            .set_len(SPARSE_BYTES)?;
        let (out, err, truncated) = read_captured_output(&stdout, &stderr, 1024)?;
        ensure!(out.len() == 1024 && err.is_empty() && truncated);
        Ok(())
    })
    .unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn sparse_output_marks_the_output_limit_exceeded() {
    let truncate = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join("truncate"))
                .find(|candidate| candidate.is_file())
        })
        .expect("test requires truncate");
    let command = vec![
        truncate.to_string_lossy().into_owned(),
        String::from("-s"),
        SPARSE_BYTES.to_string(),
        String::from("/dev/stdout"),
    ];

    let outcome = run_trusted(&command, Path::new("/"), kraai_test_support::HANG_TIMEOUT).unwrap();

    assert!(outcome.output_limit_exceeded && !outcome.success());
    assert!(outcome.stdout.len() as u64 <= MAX_CAPTURED_OUTPUT_BYTES);
}
