use std::{fs, process::Command};

#[test]
fn checking_configuration_does_not_create_runtime_data() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("douane.toml");
    fs::write(
        &config,
        "[server]\nauth_dir = 'runtime/auth'\ndatabase = 'runtime/requests.sqlite'\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_douane"))
        .args(["--config", config.to_str().unwrap(), "--check"])
        .current_dir(directory.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "configuration validation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("configuration valid"));
    assert!(
        !directory.path().join("runtime").exists(),
        "checking configuration must not create data owned by the checking user"
    );
}
