use std::process::Command;

#[test]
fn upstream_license_is_available_without_a_terminal() {
    let output = Command::new(env!("CARGO_BIN_EXE_zentop-tui"))
        .arg("--disk-license")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        include_str!("../../../licenses/disktree-MIT.md").trim()
    );
}
