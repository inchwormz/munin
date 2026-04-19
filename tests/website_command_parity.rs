const BANNED: &[&str] = &[
    "munin init",
    "munin gain",
    "munin pack",
    "munin vitest",
    "munin cargo test",
    "munin git diff",
    "munin replay-eval",
];

#[test]
fn public_docs_do_not_publish_unsupported_munin_commands() {
    let mut checked = Vec::new();
    checked.push(("README.md".to_string(), read("README.md")));

    let site_root = std::env::var("MUNIN_SITE_ROOT")
        .map(std::path::PathBuf::from)
        .ok()
        .or_else(default_site_root);
    let root = site_root.expect("MUNIN_SITE_ROOT or restored production site source is required");
    assert!(
        root.exists(),
        "site root does not exist: {}",
        root.display()
    );
    for entry in std::fs::read_dir(&root).expect("site root") {
        let entry = entry.expect("site entry");
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("html") {
            checked.push((path.display().to_string(), read(path)));
        }
    }
    if let Some(tracked_root) = tracked_site_root() {
        for file in [
            "index.html",
            "features.html",
            "docs.html",
            "resources.html",
            "pricing.html",
        ] {
            let path = tracked_root.join(file);
            if path.exists() {
                checked.push((path.display().to_string(), read(path)));
            }
        }
    }

    let mut failures = Vec::new();
    for (path, content) in checked {
        let lower = content.to_lowercase();
        for banned in BANNED {
            if lower.contains(*banned) {
                failures.push(format!("{path}: contains `{banned}`"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "public docs contain unsupported Munin promises:\n{}",
        failures.join("\n")
    );
}

fn read(path: impl AsRef<std::path::Path>) -> String {
    std::fs::read_to_string(path.as_ref())
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.as_ref().display()))
}

fn default_site_root() -> Option<std::path::PathBuf> {
    [
        r"C:\Users\OEM\Projects\munin-site\.worktrees\feat-munin-command-parity\production\muninmemory.com",
        r"C:\Users\OEM\Projects\munin-site\production\muninmemory.com",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .find(|path| path.exists())
}

fn tracked_site_root() -> Option<std::path::PathBuf> {
    [
        r"C:\Users\OEM\Projects\munin-site\.worktrees\feat-munin-command-parity",
        r"C:\Users\OEM\Projects\munin-site",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .find(|path| path.join("index.html").exists())
}
