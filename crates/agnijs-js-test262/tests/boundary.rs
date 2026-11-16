//! The engine stands alone (ADR 0016): nothing in this repository depends on
//! code outside it, and nothing in it speaks the embedding framework's
//! (Gable's) vocabulary. These tests guard the rule now that the engine lives
//! in its own repository and embedders, Gable among them, depend on it.

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
    // Gable's own crates (for example `gable-designer`) are framework code;
    // none may enter the engine's dependency graph.
    let meta = metadata();
    let offenders: Vec<String> = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["name"].as_str().unwrap().starts_with("gable"))
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

/// The crates an embedder of the engine links, by default.
const ENGINE_CRATES: &[&str] =
    &["agnijs", "agnijs-engine-api", "agnijs-js-vm", "agnijs-js-compiler", "agnijs-js-parser", "agnijs-capi"];

#[test]
fn typescript_stays_out_of_the_engine_and_the_c_api() {
    // ADR 0029: Oxc reaches only the embedders that ask for
    // agnijs-js-typescript; no engine crate depends on it, by default or
    // through any of their own features.
    let meta = metadata();
    let names: std::collections::HashMap<&str, &str> = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    for pkg in meta["packages"].as_array().unwrap() {
        let name = pkg["name"].as_str().unwrap();
        if !ENGINE_CRATES.contains(&name) {
            continue;
        }
        for dep in pkg["dependencies"].as_array().unwrap() {
            let dep_name = dep["name"].as_str().unwrap();
            let dev = dep["kind"].as_str() == Some("dev");
            assert!(
                dev || !(dep_name == "agnijs-js-typescript" || dep_name.starts_with("oxc")),
                "{name} depends on {dep_name}"
            );
        }
    }
    // And nothing reaches Oxc through the resolved default graph.
    let nodes = meta["resolve"]["nodes"].as_array().unwrap();
    let node = |id: &str| nodes.iter().find(|n| n["id"].as_str() == Some(id)).unwrap();
    let mut stack: Vec<&str> =
        names.iter().filter(|(_, n)| ENGINE_CRATES.contains(n)).map(|(id, _)| *id).collect();
    assert_eq!(stack.len(), ENGINE_CRATES.len(), "an engine crate was renamed: update ENGINE_CRATES");
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let name = names[id];
        assert!(
            name != "agnijs-js-typescript" && !name.starts_with("oxc"),
            "{name} is in the engine's default dependency graph"
        );
        for dep in node(id)["deps"].as_array().unwrap() {
            let normal = dep["dep_kinds"].as_array().unwrap().iter().any(|k| k["kind"].as_str() != Some("dev"));
            if normal {
                stack.push(dep["pkg"].as_str().unwrap());
            }
        }
    }
}
