//! Artifact-backed replay suppression for large Context outputs.

use crate::core::config::Config;
use crate::core::constants::{ARTIFACTS_DIR, LEGACY_CONTEXT_DATA_DIR, MUNIN_DATA_DIR};
use crate::core::tracking::{estimate_tokens, Tracker};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, create_dir_all, read_dir, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const ARTIFACT_ID_PREFIX: &str = "@munin/a_";
const LEGACY_ARTIFACT_ID_PREFIX: &str = "@context/a_";
const DEFAULT_HASH_PREFIX_LEN: usize = 16;
const INDEX_TAIL_CHUNK_BYTES: u64 = 16 * 1024;
const ARTIFACT_REFS_DIR: &str = "refs";
const ARTIFACT_REF_EXT: &str = "sha256";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactEventKind {
    New,
    Unchanged,
    Delta,
}

impl ArtifactEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Unchanged => "unchanged",
            Self::Delta => "delta",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub artifact_id: String,
    pub hash: String,
    pub command_sig: String,
    pub cwd: String,
    pub source_layer: String,
    pub bytes: usize,
    pub lines: usize,
    pub preview: String,
    pub created_at: String,
    pub event_type: String,
    pub previous_artifact_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ArtifactRenderResult {
    pub rendered: String,
    pub artifact_id: Option<String>,
    pub event_kind: Option<ArtifactEventKind>,
}

#[derive(Debug, Clone)]
struct ArtifactLocation {
    root: PathBuf,
    blobs_dir: PathBuf,
    refs_dir: PathBuf,
    index_path: PathBuf,
}

fn artifact_root_from_config(config: &Config) -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("MUNIN_ARTIFACTS_DIR") {
        return Ok(PathBuf::from(dir));
    }

    if let Some(dir) = &config.artifacts.directory {
        return Ok(dir.clone());
    }

    let data_dir = dirs::data_local_dir()
        .ok_or_else(|| anyhow!("failed to resolve local data directory for artifacts"))?;
    Ok(data_dir.join(MUNIN_DATA_DIR).join(ARTIFACTS_DIR))
}

fn artifact_location(config: &Config) -> Result<ArtifactLocation> {
    let root = artifact_root_from_config(config)?;
    Ok(artifact_location_from_root(root))
}

fn artifact_location_from_root(root: PathBuf) -> ArtifactLocation {
    ArtifactLocation {
        blobs_dir: root.join("blobs"),
        refs_dir: root.join(ARTIFACT_REFS_DIR),
        index_path: root.join("index.jsonl"),
        root,
    }
}

fn legacy_artifact_location_from_config(config: &Config) -> Result<Option<ArtifactLocation>> {
    if !config.artifacts.directory.is_none() || std::env::var("MUNIN_ARTIFACTS_DIR").is_ok() {
        return Ok(None);
    }
    let data_dir = dirs::data_local_dir()
        .ok_or_else(|| anyhow!("failed to resolve local data directory for artifacts"))?;
    let legacy_root = data_dir.join(LEGACY_CONTEXT_DATA_DIR).join(ARTIFACTS_DIR);
    Ok(legacy_root
        .exists()
        .then(|| artifact_location_from_root(legacy_root)))
}

fn ensure_artifact_dirs(location: &ArtifactLocation) -> Result<()> {
    create_dir_all(&location.root)?;
    create_dir_all(&location.blobs_dir)?;
    create_dir_all(&location.refs_dir)?;
    Ok(())
}

fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn artifact_id_from_hash(hash: &str) -> String {
    let prefix_len = hash.len().min(DEFAULT_HASH_PREFIX_LEN);
    format!("{}{}", ARTIFACT_ID_PREFIX, &hash[..prefix_len])
}

fn normalize_artifact_id(value: &str) -> String {
    value.trim().replace('\\', "/")
}

fn blob_path(location: &ArtifactLocation, hash: &str) -> PathBuf {
    location.blobs_dir.join(format!("{hash}.txt"))
}

fn is_valid_artifact_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn is_valid_artifact_suffix(suffix: &str) -> bool {
    suffix.len() == DEFAULT_HASH_PREFIX_LEN && suffix.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn artifact_suffix(artifact_id: &str) -> Result<String> {
    let normalized = normalize_artifact_id(artifact_id);
    let suffix = normalized
        .strip_prefix(ARTIFACT_ID_PREFIX)
        .or_else(|| normalized.strip_prefix(LEGACY_ARTIFACT_ID_PREFIX))
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("invalid artifact id: {artifact_id}"))?;

    if !is_valid_artifact_suffix(&suffix) {
        return Err(anyhow!("invalid artifact id: {artifact_id}"));
    }

    Ok(suffix)
}

fn artifact_ref_path(location: &ArtifactLocation, suffix: &str) -> PathBuf {
    location
        .refs_dir
        .join(format!("{suffix}.{ARTIFACT_REF_EXT}"))
}

fn hash_from_blob_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(ToOwned::to_owned)
}

fn store_artifact_ref(location: &ArtifactLocation, artifact_id: &str, hash: &str) -> Result<()> {
    let suffix = artifact_suffix(artifact_id)?;
    if !is_valid_artifact_hash(hash) || !hash.starts_with(&suffix) {
        return Err(anyhow!("invalid artifact hash for ref sync: {hash}"));
    }

    ensure_artifact_dirs(location)?;
    let path = artifact_ref_path(location, &suffix);
    let payload = format!("{hash}\n");
    if fs::read_to_string(&path).ok().as_deref() == Some(payload.as_str()) {
        return Ok(());
    }
    fs::write(&path, payload)
        .with_context(|| format!("failed to write artifact ref {}", path.display()))?;
    Ok(())
}

fn resolve_blob_path_from_ref(
    location: &ArtifactLocation,
    artifact_id: &str,
) -> Result<Option<PathBuf>> {
    let suffix = artifact_suffix(artifact_id)?;

    let ref_path = artifact_ref_path(location, &suffix);
    let hash = match fs::read_to_string(&ref_path) {
        Ok(hash) => hash.trim().to_string(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read artifact ref {}", ref_path.display()));
        }
    };

    if !is_valid_artifact_hash(&hash) || !hash.starts_with(&suffix) {
        return Ok(None);
    }

    let path = blob_path(location, &hash);
    if path.exists() {
        return Ok(Some(path));
    }

    Ok(None)
}

fn current_cwd_string() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn preview_text(text: &str, max_chars: usize) -> String {
    let normalized = text.trim();
    if normalized.chars().count() <= max_chars {
        return normalized.to_string();
    }

    let preview: String = normalized.chars().take(max_chars).collect();
    format!("{preview}...")
}

fn decode_index_line(bytes: &[u8]) -> Option<String> {
    let line = String::from_utf8_lossy(bytes);
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn load_recent_index_lines(index_path: &Path, limit: usize) -> Result<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut file = match File::open(index_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!("failed to open artifact index {}", index_path.display())
            });
        }
    };

    let mut cursor = file.seek(SeekFrom::End(0))?;
    if cursor == 0 {
        return Ok(Vec::new());
    }

    let mut lines = Vec::with_capacity(limit);
    let mut carry = Vec::new();

    while cursor > 0 && lines.len() < limit {
        let chunk_size = cursor.min(INDEX_TAIL_CHUNK_BYTES) as usize;
        cursor -= chunk_size as u64;
        file.seek(SeekFrom::Start(cursor))?;

        let mut chunk = vec![0; chunk_size];
        file.read_exact(&mut chunk)?;
        if !carry.is_empty() {
            chunk.extend_from_slice(&carry);
        }

        let mut line_end = chunk.len();
        for index in (0..chunk.len()).rev() {
            if chunk[index] != b'\n' {
                continue;
            }

            if let Some(line) = decode_index_line(&chunk[index + 1..line_end]) {
                lines.push(line);
                if lines.len() == limit {
                    break;
                }
            }
            line_end = index;
        }

        carry = chunk[..line_end].to_vec();
    }

    if lines.len() < limit {
        if let Some(line) = decode_index_line(&carry) {
            lines.push(line);
        }
    }

    Ok(lines)
}

fn load_recent_records(location: &ArtifactLocation, limit: usize) -> Vec<ArtifactRecord> {
    load_recent_index_lines(&location.index_path, limit)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|line| serde_json::from_str::<ArtifactRecord>(&line).ok())
        .collect()
}

fn summarize_delta(previous: &str, current: &str, max_lines: usize) -> String {
    let previous_lines: Vec<&str> = previous.lines().collect();
    let current_lines: Vec<&str> = current.lines().collect();

    let mut removed: Vec<&str> = Vec::new();
    for line in &previous_lines {
        if !current_lines.contains(line) {
            removed.push(line);
        }
    }

    let mut added: Vec<&str> = Vec::new();
    for line in &current_lines {
        if !previous_lines.contains(line) {
            added.push(line);
        }
    }

    let mut summary = Vec::new();
    let half = (max_lines / 2).max(1);
    for line in removed.iter().take(half) {
        summary.push(format!("- {}", line.trim_end()));
    }
    for line in added.iter().take(half) {
        summary.push(format!("+ {}", line.trim_end()));
    }

    if summary.is_empty() {
        return "content changed but no concise delta summary was available".to_string();
    }

    summary.join("\n")
}

fn render_new_output(
    artifact_id: &str,
    text: &str,
    bytes: usize,
    lines: usize,
    preview_chars: usize,
) -> String {
    format!(
        "[artifact {artifact_id}]\nstats: {bytes} bytes | {lines} lines\npreview:\n{}\n[use: munin show {artifact_id}]",
        preview_text(text, preview_chars)
    )
}

fn render_unchanged_output(artifact_id: &str, bytes: usize, lines: usize) -> String {
    format!(
        "[artifact {artifact_id} unchanged since previous run]\nstats: {bytes} bytes | {lines} lines\n[use: munin show {artifact_id}]"
    )
}

fn render_delta_output(
    artifact_id: &str,
    previous_artifact_id: &str,
    previous_text: &str,
    text: &str,
    bytes: usize,
    lines: usize,
    delta_lines: usize,
) -> String {
    format!(
        "[artifact {artifact_id} changed since {previous_artifact_id}]\nstats: {bytes} bytes | {lines} lines\ndelta summary:\n{}\n[use: munin diff {previous_artifact_id} {artifact_id}]",
        summarize_delta(previous_text, text, delta_lines)
    )
}

fn append_record(location: &ArtifactLocation, record: &ArtifactRecord) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&location.index_path)
        .with_context(|| {
            format!(
                "failed to open artifact index {}",
                location.index_path.display()
            )
        })?;
    writeln!(file, "{}", serde_json::to_string(record)?)?;
    Ok(())
}

fn store_blob_if_missing(location: &ArtifactLocation, hash: &str, text: &str) -> Result<()> {
    let path = blob_path(location, hash);
    if !path.exists() {
        std::fs::write(&path, text)
            .with_context(|| format!("failed to write artifact blob {}", path.display()))?;
    }
    Ok(())
}

pub fn is_artifact_id(value: &str) -> bool {
    artifact_suffix(value).is_ok()
}

pub fn prepare_output_for_display(
    command_sig: &str,
    text: &str,
    source_layer: &str,
) -> Result<ArtifactRenderResult> {
    let config = Config::load()?;
    prepare_output_for_display_with_config(
        command_sig,
        text,
        source_layer,
        &config,
        &current_cwd_string(),
    )
}

fn prepare_output_for_display_with_config(
    command_sig: &str,
    text: &str,
    source_layer: &str,
    config: &Config,
    cwd: &str,
) -> Result<ArtifactRenderResult> {
    if !config.artifacts.enabled || std::env::var("MUNIN_ARTIFACTS").ok().as_deref() == Some("0") {
        return Ok(ArtifactRenderResult {
            rendered: text.to_string(),
            artifact_id: None,
            event_kind: None,
        });
    }

    let bytes = text.len();
    let lines = text.lines().count();
    if bytes < config.artifacts.min_chars && lines < config.artifacts.min_lines {
        return Ok(ArtifactRenderResult {
            rendered: text.to_string(),
            artifact_id: None,
            event_kind: None,
        });
    }

    let location = artifact_location(config)?;
    ensure_artifact_dirs(&location)?;

    let hash = hash_text(text);
    let artifact_id = artifact_id_from_hash(&hash);
    store_blob_if_missing(&location, &hash, text)?;
    store_artifact_ref(&location, &artifact_id, &hash)?;

    let recent = load_recent_records(&location, config.artifacts.recent_lookup_limit);
    let previous_same_command = recent
        .iter()
        .find(|record| record.command_sig == command_sig && record.cwd == cwd);
    let previous_same_hash = recent.iter().find(|record| record.hash == hash);

    let (event_kind, rendered, previous_artifact_id) = if let Some(previous) = previous_same_command
    {
        if previous.hash == hash {
            (
                ArtifactEventKind::Unchanged,
                render_unchanged_output(&artifact_id, bytes, lines),
                Some(previous.artifact_id.clone()),
            )
        } else {
            let previous_text =
                std::fs::read_to_string(blob_path(&location, &previous.hash)).unwrap_or_default();
            let previous_id = previous.artifact_id.clone();
            (
                ArtifactEventKind::Delta,
                render_delta_output(
                    &artifact_id,
                    &previous_id,
                    &previous_text,
                    text,
                    bytes,
                    lines,
                    config.artifacts.delta_lines,
                ),
                Some(previous_id),
            )
        }
    } else if let Some(previous) = previous_same_hash {
        (
            ArtifactEventKind::Unchanged,
            render_unchanged_output(&artifact_id, bytes, lines),
            Some(previous.artifact_id.clone()),
        )
    } else {
        (
            ArtifactEventKind::New,
            render_new_output(
                &artifact_id,
                text,
                bytes,
                lines,
                config.artifacts.preview_chars,
            ),
            None,
        )
    };

    let record = ArtifactRecord {
        artifact_id: artifact_id.clone(),
        hash,
        command_sig: command_sig.to_string(),
        cwd: cwd.to_string(),
        source_layer: source_layer.to_string(),
        bytes,
        lines,
        preview: preview_text(text, config.artifacts.preview_chars),
        created_at: Utc::now().to_rfc3339(),
        event_type: event_kind.as_str().to_string(),
        previous_artifact_id,
    };
    append_record(&location, &record)?;

    let rendered_tokens = estimate_tokens(&rendered);
    let source_tokens = estimate_tokens(text);
    if let Ok(tracker) = Tracker::new() {
        let _ = tracker.record_artifact_event(
            command_sig,
            &artifact_id,
            source_layer,
            event_kind.as_str(),
            source_tokens,
            rendered_tokens,
        );
    }

    Ok(ArtifactRenderResult {
        rendered,
        artifact_id: Some(artifact_id),
        event_kind: Some(event_kind),
    })
}

fn resolve_blob_path_from_artifact_id_with_location(
    location: &ArtifactLocation,
    artifact_id: &str,
    reseed_ref: bool,
) -> Result<PathBuf> {
    let normalized = normalize_artifact_id(artifact_id);
    let prefix = artifact_suffix(&normalized)?;

    if let Some(path) = resolve_blob_path_from_ref(location, &normalized)? {
        return Ok(path);
    }

    let mut matches = Vec::new();
    let entries = match read_dir(&location.blobs_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(anyhow!("artifact not found: {artifact_id}"));
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "failed to read artifact blob directory {}",
                    location.blobs_dir.display()
                )
            });
        }
    };

    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.starts_with(&prefix) {
            matches.push(entry.path());
        }
    }

    match matches.len() {
        0 => Err(anyhow!("artifact not found: {artifact_id}")),
        1 => {
            let path = matches.remove(0);
            if reseed_ref {
                if let Some(hash) = hash_from_blob_path(&path) {
                    let _ = store_artifact_ref(location, &normalized, &hash);
                }
            }
            Ok(path)
        }
        _ => Err(anyhow!("artifact id is ambiguous: {artifact_id}")),
    }
}

fn resolve_blob_path_from_artifact_id_with_config(
    config: &Config,
    artifact_id: &str,
) -> Result<PathBuf> {
    let primary = artifact_location(config)?;
    let legacy = legacy_artifact_location_from_config(config)?;
    resolve_blob_path_from_artifact_id_with_locations(&primary, legacy.as_ref(), artifact_id)
}

fn resolve_blob_path_from_artifact_id_with_locations(
    primary: &ArtifactLocation,
    legacy: Option<&ArtifactLocation>,
    artifact_id: &str,
) -> Result<PathBuf> {
    match resolve_blob_path_from_artifact_id_with_location(primary, artifact_id, true) {
        Ok(path) => Ok(path),
        Err(primary_err)
            if normalize_artifact_id(artifact_id).starts_with(LEGACY_ARTIFACT_ID_PREFIX) =>
        {
            if let Some(legacy) = legacy {
                if legacy.root != primary.root {
                    return resolve_blob_path_from_artifact_id_with_location(
                        legacy,
                        artifact_id,
                        false,
                    )
                        .with_context(|| {
                            format!(
                                "legacy artifact fallback also failed after primary lookup error: {primary_err:#}"
                            )
                        });
                }
            }
            Err(primary_err)
        }
        Err(err) => Err(err),
    }
}

pub fn load_artifact_text(artifact_id: &str) -> Result<String> {
    let config = Config::load()?;
    load_artifact_text_with_config(&config, artifact_id)
}

pub(crate) fn load_artifact_text_with_config(config: &Config, artifact_id: &str) -> Result<String> {
    let path = resolve_blob_path_from_artifact_id_with_config(config, artifact_id)?;
    std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read artifact {}", path.display()))
}

#[cfg(test)]
pub(crate) fn prepare_output_for_display_for_test(
    command_sig: &str,
    text: &str,
    source_layer: &str,
    config: &Config,
    cwd: &str,
) -> Result<ArtifactRenderResult> {
    prepare_output_for_display_with_config(command_sig, text, source_layer, config, cwd)
}

fn parse_line_range(spec: &str) -> Result<(usize, usize)> {
    let (start, end) = spec
        .split_once(':')
        .ok_or_else(|| anyhow!("line range must use start:end"))?;
    let start_num = start
        .trim()
        .parse::<usize>()
        .context("invalid line range start")?;
    let end_num = end
        .trim()
        .parse::<usize>()
        .context("invalid line range end")?;
    if start_num == 0 || end_num == 0 || end_num < start_num {
        return Err(anyhow!("line range must be 1-based and end >= start"));
    }
    Ok((start_num, end_num))
}

pub fn show_artifact(artifact_id: &str, lines_spec: Option<&str>) -> Result<String> {
    let config = Config::load()?;
    show_artifact_with_config(&config, artifact_id, lines_spec)
}

fn show_artifact_with_config(
    config: &Config,
    artifact_id: &str,
    lines_spec: Option<&str>,
) -> Result<String> {
    let path = resolve_blob_path_from_artifact_id_with_config(config, artifact_id)?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read artifact {}", path.display()))?;
    if let Some(spec) = lines_spec {
        let (start, end) = parse_line_range(spec)?;
        let selected = text
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                let line_no = index + 1;
                if line_no < start || line_no > end {
                    None
                } else {
                    Some(format!("{line_no:>6} {line}"))
                }
            })
            .collect::<Vec<_>>();
        return Ok(selected.join("\n"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_config() -> (TempDir, Config) {
        let tmp = TempDir::new().expect("temp dir");
        let mut config = Config::default();
        config.artifacts.directory = Some(tmp.path().join("artifacts"));
        (tmp, config)
    }

    #[test]
    fn artifact_id_is_stable_for_identical_output() {
        let (_tmp, config) = temp_config();
        let output = "same output body\n".repeat(200);
        let first = prepare_output_for_display_with_config(
            "munin grep",
            &output,
            "runner",
            &config,
            "C:/repo",
        )
        .expect("first");
        let second = prepare_output_for_display_with_config(
            "munin grep",
            &output,
            "runner",
            &config,
            "C:/repo",
        )
        .expect("second");
        assert_eq!(first.artifact_id, second.artifact_id);
        assert_eq!(second.event_kind, Some(ArtifactEventKind::Unchanged));
    }

    #[test]
    fn show_artifact_returns_line_slice() {
        let (_tmp, config) = temp_config();
        let output = (1..=10)
            .map(|i| format!("line-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let rendered = prepare_output_for_display_with_config(
            "munin grep",
            &output.repeat(100),
            "runner",
            &config,
            "C:/repo",
        )
        .expect("rendered");
        let id = rendered.artifact_id.expect("artifact id");
        let shown = show_artifact_with_config(&config, &id, Some("2:4")).expect("show");
        assert!(shown.contains("     2 line-2"));
        assert!(shown.contains("     4 line-4"));
    }

    #[test]
    fn backslash_artifact_ids_resolve_like_windows_paths() {
        let (_tmp, config) = temp_config();
        let output = "line\n".repeat(200);
        let rendered = prepare_output_for_display_with_config(
            "munin read sample.txt",
            &output,
            "runner",
            &config,
            "C:/repo",
        )
        .expect("rendered");
        let artifact_id = rendered.artifact_id.expect("artifact id");
        let windows_style_id = artifact_id.replace('/', "\\");

        assert!(is_artifact_id(&windows_style_id));
        let loaded = load_artifact_text_with_config(&config, &windows_style_id).expect("load");
        assert_eq!(loaded, output);
    }

    #[test]
    fn exact_artifact_resolution_reseeds_missing_ref_file() {
        let (_tmp, config) = temp_config();
        let output = "line\n".repeat(200);
        let rendered = prepare_output_for_display_with_config(
            "munin read sample.txt",
            &output,
            "runner",
            &config,
            "C:/repo",
        )
        .expect("rendered");
        let artifact_id = rendered.artifact_id.expect("artifact id");
        let location = artifact_location(&config).expect("artifact location");
        let suffix = artifact_suffix(&artifact_id).expect("artifact suffix");
        let ref_path = artifact_ref_path(&location, &suffix);

        fs::remove_file(&ref_path).expect("remove ref");
        assert!(!ref_path.exists(), "test requires missing ref sidecar");

        let loaded = load_artifact_text_with_config(&config, &artifact_id).expect("load");
        assert_eq!(loaded, output);
        assert!(ref_path.exists(), "load should reseed the ref sidecar");
    }

    #[test]
    fn corrupt_ref_sidecar_falls_back_to_blob_scan_and_reseeds() {
        let (_tmp, config) = temp_config();
        let output = "line\n".repeat(200);
        let rendered = prepare_output_for_display_with_config(
            "munin read sample.txt",
            &output,
            "runner",
            &config,
            "C:/repo",
        )
        .expect("rendered");
        let artifact_id = rendered.artifact_id.expect("artifact id");
        let location = artifact_location(&config).expect("artifact location");
        let suffix = artifact_suffix(&artifact_id).expect("artifact suffix");
        let ref_path = artifact_ref_path(&location, &suffix);

        fs::write(&ref_path, "not-a-valid-hash\n").expect("corrupt ref");
        let loaded = load_artifact_text_with_config(&config, &artifact_id).expect("load");
        assert_eq!(loaded, output);

        let ref_contents = fs::read_to_string(&ref_path).expect("ref contents");
        assert!(ref_contents.trim().starts_with(&suffix));
    }

    #[test]
    fn invalid_artifact_ids_do_not_accept_path_segments() {
        assert!(!is_artifact_id("@munin/a_deadbeefdeadbeef/extra"));
        assert!(!is_artifact_id("@context/a_deadbeefdeadbeef/extra"));
        assert!(!is_artifact_id("@context\\a_deadbeefdeadbeef\\extra"));
        assert!(artifact_suffix("@munin/a_deadbeefdeadbeef/extra").is_err());
        assert!(artifact_suffix("@context/a_deadbeefdeadbeef/extra").is_err());
    }

    #[test]
    fn legacy_context_artifact_ids_still_load() {
        let (_tmp, config) = temp_config();
        let output = "line\n".repeat(200);
        let rendered = prepare_output_for_display_with_config(
            "munin read sample.txt",
            &output,
            "runner",
            &config,
            "C:/repo",
        )
        .expect("rendered");
        let artifact_id = rendered.artifact_id.expect("artifact id");
        let legacy_id = artifact_id.replace("@munin/", "@context/");

        assert!(is_artifact_id(&legacy_id));
        let loaded = load_artifact_text_with_config(&config, &legacy_id).expect("legacy load");
        assert_eq!(loaded, output);
    }

    #[test]
    fn legacy_context_artifact_loads_from_legacy_root_when_munin_root_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_root = tmp.path();
        let munin_root = data_root.join(MUNIN_DATA_DIR).join(ARTIFACTS_DIR);
        let legacy_root = data_root.join(LEGACY_CONTEXT_DATA_DIR).join(ARTIFACTS_DIR);
        fs::create_dir_all(munin_root.join("blobs")).expect("munin blobs");
        fs::create_dir_all(legacy_root.join("blobs")).expect("legacy blobs");
        fs::create_dir_all(legacy_root.join(ARTIFACT_REFS_DIR)).expect("legacy refs");

        let output = "legacy artifact text\n".repeat(50);
        let hash = hash_text(&output);
        let legacy_id = format!(
            "{}{}",
            LEGACY_ARTIFACT_ID_PREFIX,
            &hash[..DEFAULT_HASH_PREFIX_LEN]
        );
        fs::write(
            legacy_root.join("blobs").join(format!("{hash}.txt")),
            &output,
        )
        .expect("legacy blob");

        let primary = artifact_location_from_root(munin_root);
        let legacy = artifact_location_from_root(legacy_root);
        let path =
            resolve_blob_path_from_artifact_id_with_locations(&primary, Some(&legacy), &legacy_id)
                .expect("legacy path");
        let loaded = fs::read_to_string(path).expect("legacy load");
        let suffix = artifact_suffix(&legacy_id).expect("legacy suffix");
        let legacy_ref_path = artifact_ref_path(&legacy, &suffix);

        assert_eq!(loaded, output);
        assert!(
            !legacy_ref_path.exists(),
            "legacy artifact fallback must stay read-only"
        );
    }

    #[test]
    fn load_recent_records_reads_tail_when_index_has_no_trailing_newline() {
        let (_tmp, config) = temp_config();
        let location = artifact_location(&config).expect("artifact location");
        ensure_artifact_dirs(&location).expect("artifact dirs");

        let records = (0..6)
            .map(|index| ArtifactRecord {
                artifact_id: format!("@munin/a_{index:016x}"),
                hash: format!("{index:064x}"),
                command_sig: format!("cmd-{index}"),
                cwd: "C:/repo".to_string(),
                source_layer: "runner".to_string(),
                bytes: index + 1,
                lines: index + 2,
                preview: format!("preview-{index}"),
                created_at: format!("2026-04-11T00:00:0{index}Z"),
                event_type: "new".to_string(),
                previous_artifact_id: None,
            })
            .collect::<Vec<_>>();

        let serialized = records
            .iter()
            .map(|record| serde_json::to_string(record).expect("serialize"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&location.index_path, serialized).expect("write index");

        let recent = load_recent_records(&location, 3);
        let commands = recent
            .iter()
            .map(|record| record.command_sig.as_str())
            .collect::<Vec<_>>();

        assert_eq!(commands, vec!["cmd-5", "cmd-4", "cmd-3"]);
    }
}
