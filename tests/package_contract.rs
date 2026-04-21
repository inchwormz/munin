#[test]
fn cargo_toml_marks_crate_apache_licensed() {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("Cargo.toml");
    assert!(
        manifest
            .lines()
            .any(|line| line.trim() == r#"license = "Apache-2.0""#),
        "Cargo.toml must declare the Apache-2.0 license"
    );
    assert!(
        !manifest
            .lines()
            .any(|line| line.trim() == "publish = false"),
        "Munin should be publishable during the 0.5 customer-testing cutover"
    );
}

#[test]
fn readme_names_open_source_license() {
    let readme = std::fs::read_to_string("README.md").expect("README.md");
    assert!(readme.contains("Apache 2.0"));
}

#[test]
fn release_check_script_is_packaged_with_readme() {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("Cargo.toml");
    let script_path = "/scripts/munin-release-check.ps1";
    assert!(
        manifest.contains(script_path),
        "Cargo.toml include list must package {script_path}"
    );
    assert!(
        std::path::Path::new("scripts/munin-release-check.ps1").exists(),
        "README documents scripts/munin-release-check.ps1, so it must exist"
    );
}
