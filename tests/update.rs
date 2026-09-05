#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn update_case(download: bool, binary: bool, source: bool) -> (bool, String) {
    let root = std::env::temp_dir().join(format!(
        "ppx-update-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(root.join("tools")).unwrap();
    fs::create_dir(root.join("tmp")).unwrap();
    fs::create_dir(root.join("destination")).unwrap();
    let curl = root.join("tools/curl");
    fs::write(
        &curl,
        format!("#!/bin/sh\nexit {}\n", if download { 0 } else { 1 }),
    )
    .unwrap();
    let bash = root.join("tools/bash");
    fs::write(&bash, format!(
        "#!/bin/sh\nprintf '%s|%s\\n' \"$*\" \"$PPX_INSTALL_DIR\" >> \"$UPDATE_LOG\"\ncase \"$*\" in\n *'--method binary') exit {};;\n *'--method source') exit {};;\n *) exit 9;;\nesac\n",
        if binary { 0 } else { 1 }, if source { 0 } else { 1 }
    )).unwrap();
    for file in [&curl, &bash] {
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_ppx"))
        .arg("update")
        .env("PATH", root.join("tools"))
        .env("TMPDIR", root.join("tmp"))
        .env("PPX_INSTALL_DIR", root.join("destination"))
        .env("UPDATE_LOG", root.join("calls"))
        .output()
        .unwrap();
    let calls = fs::read_to_string(root.join("calls")).unwrap_or_default();
    assert_eq!(fs::read_dir(root.join("tmp")).unwrap().count(), 0);
    for call in calls.lines() {
        assert!(call.ends_with(root.join("destination").to_str().unwrap()));
        assert!(call.contains("--no-firewall"));
    }
    fs::remove_dir_all(root).unwrap();
    (output.status.success(), calls)
}

#[test]
fn binary_update_succeeds_without_source_build() {
    let (success, calls) = update_case(true, true, false);
    assert!(success);
    assert_eq!(calls.lines().count(), 1);
}

#[test]
fn missing_binary_falls_back_to_source_in_same_destination() {
    let (success, calls) = update_case(true, false, true);
    assert!(success);
    assert_eq!(calls.lines().count(), 2);
    assert!(calls.contains("--method source"));
}

#[test]
fn failed_updates_return_failure() {
    assert!(!update_case(true, false, false).0);
    let (success, calls) = update_case(false, false, false);
    assert!(!success);
    assert!(calls.is_empty());
}
