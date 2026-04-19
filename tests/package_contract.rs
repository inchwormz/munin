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
