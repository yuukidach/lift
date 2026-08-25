#[test]
fn lift_cli_help_uses_lift_brand() {
    let output = test_bin::get_test_bin!("lift-cli").arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Command-line interface for Lift window manager"));
}

#[test]
fn lift_agent_help_uses_lift_command_name() {
    let output = test_bin::get_test_bin!("lift").arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Usage: lift"));
}

#[test]
fn lift_binaries_report_the_package_version() {
    fn assert_version(binary: &str, mut command: std::process::Command) {
        let output = command.arg("--version").output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            format!("{binary} {}", env!("CARGO_PKG_VERSION"))
        );
    }

    assert_version("lift", test_bin::get_test_bin!("lift"));
    assert_version("lift-cli", test_bin::get_test_bin!("lift-cli"));
}
