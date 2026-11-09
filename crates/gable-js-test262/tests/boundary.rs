//! The engine workspace stands alone (ADR 0016): nothing under
//! `native/engine` depends on code outside it, and nothing in it speaks the
//! embedding framework's vocabulary. These tests guard the rule so the later
//! repository split stays mechanical.

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn metadata() -> serde_json::Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(workspace_root())
        .output()
        .expect("run cargo metadata");
    assert!(out.status.success(), "cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn every_path_dependency_is_a_member_of_the_engine_workspace() {
    let meta = metadata();
    let root = workspace_root();
    let members: Vec<&str> = meta["workspace_members"].as_array().unwrap().iter().map(|m| m.as_str().unwrap()).collect();
    let mut offenders = Vec::new();
    for pkg in meta["packages"].as_array().unwrap() {
        let name = pkg["name"].as_str().unwrap();
        let id = pkg["id"].as_str().unwrap();
        let manifest = PathBuf::from(pkg["manifest_path"].as_str().unwrap());
        let manifest = manifest.canonicalize().unwrap_or(manifest);
        if pkg["source"].is_null() && (!members.contains(&id) || !manifest.starts_with(&root)) {
            offenders.push(format!("{name} ({})", manifest.display()));
        }
    }
    assert!(offenders.is_empty(), "path dependencies outside the engine workspace: {offenders:?}");
}

#[test]
fn no_dependency_is_a_crate_of_the_embedding_framework() {
    // Gable's own crates (for example `gable-designer`) share the `gable`
    // prefix with the engine's working name; any such crate that is not a
    // workspace member is framework code reaching into the engine.
    let meta = metadata();
    let members: Vec<&str> = meta["workspace_members"].as_array().unwrap().iter().map(|m| m.as_str().unwrap()).collect();
    let offenders: Vec<String> = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["name"].as_str().unwrap().starts_with("gable") && !members.contains(&p["id"].as_str().unwrap()))
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    assert!(offenders.is_empty(), "framework crates in the engine's dependency graph: {offenders:?}");
}

/// Terms that only make sense inside the embedding framework (its glossary,
/// packages and products). The engine talks about "the host" or "the
/// embedder" instead.
const FRAMEWORK_TERMS: &[&str] = &[
    "host runtime",
    "renderer",
    "@gable/",
    "gable-ui",
    "gable native",
    "gable-designer",
    "template interpreter",
    "error handler",
    "gable://",
];

#[test]
fn engine_sources_do_not_use_framework_vocabulary() {
    let crates = workspace_root().join("crates");
    let mut offenders = Vec::new();
    for entry in walkdir::WalkDir::new(&crates).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !entry.file_type().is_file() || !matches!(ext, "rs" | "toml" | "js") || path.ends_with("tests/boundary.rs") {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap().to_lowercase();
        for (n, line) in text.lines().enumerate() {
            for term in FRAMEWORK_TERMS {
                if line.contains(term) {
                    offenders.push(format!("{}:{}: {term}", path.strip_prefix(&crates).unwrap().display(), n + 1));
                }
            }
        }
    }
    assert!(offenders.is_empty(), "framework vocabulary in engine sources:\n{}", offenders.join("\n"));
}
