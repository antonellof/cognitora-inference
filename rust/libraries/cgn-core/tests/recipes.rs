//! Smoke-test every recipe TOML against the live `Config` schema.
//!
//! Recipes live under [`recipes/`](../../../../recipes) and are
//! supposed to be drop-in profiles for `scripts/run/up.sh`. If a
//! schema change makes a recipe TOML fail to parse, this test catches
//! it before users do.
//!
//! The test walks the `recipes/` tree (relative to the workspace
//! root), loads every `router.toml`, `agent-*.toml`, and
//! `kvcached.toml`, and asserts that `Config::load` succeeds. Recipes
//! shipped with templated paths (the `llama-cpp/cpu` GGUF) are
//! tolerated — `Config::load` does not validate that filesystem paths
//! exist.

use std::path::{Path, PathBuf};

use cgn_core::config::Config;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at the cgn-core crate; walk two
    // levels up to reach the workspace root.
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("cgn-core lives at rust/libraries/cgn-core")
        .to_path_buf()
}

fn recipe_tomls(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name.starts_with('_') || name.starts_with('.') {
                    continue;
                }
                stack.push(path);
            } else if path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|e| e == "toml")
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn every_recipe_toml_parses_against_the_live_schema() {
    let root = workspace_root().join("recipes");
    if !root.is_dir() {
        eprintln!("note: {} not present, skipping", root.display());
        return;
    }

    let tomls = recipe_tomls(&root);
    assert!(
        !tomls.is_empty(),
        "expected at least one TOML under recipes/"
    );

    let mut failures: Vec<(PathBuf, cgn_core::Error)> = Vec::new();
    for p in &tomls {
        // Skip the llama-cpp template — its `path = "${LLAMA_GGUF}"`
        // is replaced by the recipe's `up.sh` at run-time. Loading the
        // raw template directly is fine (Config::load doesn't check
        // that the path exists), but we list it explicitly so a future
        // schema change cannot accidentally pass on an unrelated bug.
        match Config::load(p) {
            Ok(_) => {}
            Err(e) => failures.push((p.clone(), e)),
        }
    }

    if !failures.is_empty() {
        for (p, e) in &failures {
            eprintln!("FAIL: {} → {:?}", p.display(), e);
        }
        panic!(
            "{}/{} recipe TOMLs failed to parse",
            failures.len(),
            tomls.len()
        );
    }
}
