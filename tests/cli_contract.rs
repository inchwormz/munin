use std::process::Command;

fn munin(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_munin"))
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("failed to run munin {args:?}: {error}"));
    assert!(
        output.status.success(),
        "munin {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn resolve_routes_friction_question() {
    let output = munin(&[
        "resolve", "--format", "json", "what", "keeps", "going", "wrong",
    ]);
    assert!(output.contains("\"route\": \"friction\""));
    assert!(output.contains("munin friction --last 30d --format text"));
}

#[test]
fn generated_skills_and_resolver_targets_are_parseable() {
    let output = munin(&["install", "--check-resolvable"]);
    assert!(output.contains("resolver, skill, and fixture checks passed"));
}
