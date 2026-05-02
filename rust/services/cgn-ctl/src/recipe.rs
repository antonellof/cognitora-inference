//! `cgn-ctl recipe …` — one-line bring-up for production model recipes.
//!
//! A recipe is a folder of TOML profiles plus a tiny `up.sh` driver. The
//! recipes ship in-tree under `recipes/` and mirror the layout used by
//! NVIDIA Dynamo (one folder per model × engine × topology), but adapt
//! to Cognitora's profile-driven runtime: there is no Python framework
//! and no CRD to install — every recipe is just a flat folder of
//! TOMLs.
//!
//! ```text
//! cgn-ctl recipe ls                         # list all in-tree recipes
//! cgn-ctl recipe show llama3-8b/vllm/agg    # print metadata + files
//! cgn-ctl recipe up   llama3-8b/vllm/agg    # spin up the cluster
//! cgn-ctl recipe down llama3-8b/vllm/agg    # tear it down
//! ```
//!
//! `recipe up` invokes the recipe's `up.sh`, which sources
//! `recipes/_lib/recipe.sh` and runs `scripts/run/up.sh` on the
//! profile. `recipe down` invokes `scripts/run/down.sh`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cgn_core::{Error, Result};
use clap::Subcommand;
use tracing::info;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// List all recipes shipped under `recipes/`.
    Ls {
        /// Override the recipes root (default: `<repo-root>/recipes`).
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Print a recipe's README and the files it contains.
    Show {
        /// Recipe name, e.g. `llama3-8b/vllm/agg`.
        name: String,
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Bring up a recipe's full Cognitora cluster (router + agents + KV).
    Up {
        /// Recipe name, e.g. `llama3-8b/vllm/agg`.
        name: String,
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Tear down a previously-started recipe.
    Down {
        /// Recipe name, e.g. `llama3-8b/vllm/agg`.
        name: String,
        #[arg(long)]
        root: Option<PathBuf>,
    },
}

pub async fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Ls { root } => ls(root),
        Cmd::Show { name, root } => show(&name, root),
        Cmd::Up { name, root } => up(&name, root),
        Cmd::Down { name, root } => down(&name, root),
    }
}

/// Locate the recipes root.
///
/// 1. Explicit `--root` flag.
/// 2. `$CGN_RECIPES_DIR`.
/// 3. `<repo-root>/recipes` (resolved by walking upward from the binary
///    and from `cwd`, looking for a `recipes/_lib/recipe.sh` marker).
fn resolve_root(arg: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = arg {
        if !p.is_dir() {
            return Err(Error::Config(format!(
                "--root {} is not a directory",
                p.display()
            )));
        }
        return Ok(p);
    }
    if let Ok(env) = std::env::var("CGN_RECIPES_DIR") {
        let p = PathBuf::from(env);
        if p.is_dir() {
            return Ok(p);
        }
    }
    if let Some(p) = walk_up_for_recipes() {
        return Ok(p);
    }
    Err(Error::Config(
        "could not locate the recipes/ directory; pass --root or set CGN_RECIPES_DIR".into(),
    ))
}

fn walk_up_for_recipes() -> Option<PathBuf> {
    let mut bases = Vec::new();
    if let Ok(p) = std::env::current_dir() {
        bases.push(p);
    }
    if let Ok(p) = std::env::current_exe() {
        if let Some(parent) = p.parent() {
            bases.push(parent.to_path_buf());
        }
    }
    for base in bases {
        let mut here: Option<&Path> = Some(&base);
        while let Some(d) = here {
            let candidate = d.join("recipes").join("_lib").join("recipe.sh");
            if candidate.is_file() {
                return Some(d.join("recipes"));
            }
            here = d.parent();
        }
    }
    None
}

fn recipe_dir(root: &Path, name: &str) -> Result<PathBuf> {
    let p = root.join(name);
    if !p.is_dir() {
        return Err(Error::Config(format!(
            "no recipe `{}` (looked under {})",
            name,
            root.display()
        )));
    }
    if !p.join("up.sh").is_file() {
        return Err(Error::Config(format!(
            "{} does not look like a recipe (no up.sh)",
            p.display()
        )));
    }
    Ok(p)
}

fn ls(root: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(root)?;
    let mut found: Vec<String> = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('_') || name.starts_with('.') {
                continue;
            }
            if path.join("up.sh").is_file() {
                if let Ok(rel) = path.strip_prefix(&root) {
                    found.push(rel.display().to_string());
                }
            } else {
                stack.push(path);
            }
        }
    }
    found.sort();
    if found.is_empty() {
        println!("(no recipes under {})", root.display());
    } else {
        println!("recipes (root: {})", root.display());
        for r in found {
            println!("  {r}");
        }
    }
    Ok(())
}

fn show(name: &str, root: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(root)?;
    let dir = recipe_dir(&root, name)?;
    println!("recipe: {}", name);
    println!("path:   {}", dir.display());
    if let Ok(readme) = std::fs::read_to_string(dir.join("README.md")) {
        println!("\n{readme}");
    }
    println!("\nfiles:");
    for entry in std::fs::read_dir(&dir).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let n = entry.file_name();
        println!("  {}", n.to_string_lossy());
    }
    Ok(())
}

fn up(name: &str, root: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(root)?;
    let dir = recipe_dir(&root, name)?;
    info!(recipe = %name, path = %dir.display(), "recipe up");
    let status = Command::new("bash")
        .arg(dir.join("up.sh"))
        .stdin(Stdio::null())
        .status()
        .map_err(|e| Error::Internal(format!("spawn bash: {e}")))?;
    if !status.success() {
        return Err(Error::Internal(format!(
            "recipe up exited with status {status}"
        )));
    }
    Ok(())
}

fn down(name: &str, root: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(root)?;
    let _ = recipe_dir(&root, name)?;
    let down_sh = root
        .parent()
        .map(|p| p.join("scripts").join("run").join("down.sh"))
        .ok_or_else(|| Error::Config("recipes root has no parent (cannot find down.sh)".into()))?;
    if !down_sh.is_file() {
        return Err(Error::Config(format!(
            "down.sh not found at {}",
            down_sh.display()
        )));
    }
    info!(recipe = %name, "recipe down");
    let status = Command::new("bash")
        .arg(down_sh)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| Error::Internal(format!("spawn bash: {e}")))?;
    if !status.success() {
        return Err(Error::Internal(format!(
            "recipe down exited with status {status}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_recipes(dir: &Path, recipes: &[&str]) {
        std::fs::create_dir_all(dir.join("_lib")).unwrap();
        std::fs::write(dir.join("_lib").join("recipe.sh"), "# stub").unwrap();
        for r in recipes {
            let p = dir.join(r);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("up.sh"), "#!/bin/sh\necho up\n").unwrap();
            std::fs::write(p.join("README.md"), format!("# {r}\n")).unwrap();
        }
    }

    #[test]
    fn ls_finds_recipes() {
        let td = TempDir::new().unwrap();
        let root = td.path().join("recipes");
        std::fs::create_dir_all(&root).unwrap();
        make_recipes(
            &root,
            &[
                "llama3-8b/vllm/agg",
                "llama3-8b/sglang/agg",
                "qwen3-7b/vllm/agg",
            ],
        );
        // Just ensure no panic and the directory enumeration sees the
        // up.sh markers.
        ls(Some(root.clone())).unwrap();
    }

    #[test]
    fn show_prints_metadata() {
        let td = TempDir::new().unwrap();
        let root = td.path().join("recipes");
        std::fs::create_dir_all(&root).unwrap();
        make_recipes(&root, &["llama3-8b/vllm/agg"]);
        show("llama3-8b/vllm/agg", Some(root)).unwrap();
    }

    #[test]
    fn unknown_recipe_errors_clearly() {
        let td = TempDir::new().unwrap();
        let root = td.path().join("recipes");
        std::fs::create_dir_all(&root).unwrap();
        make_recipes(&root, &["llama3-8b/vllm/agg"]);
        let e = show("does-not-exist", Some(root)).unwrap_err();
        assert!(format!("{e:?}").contains("no recipe"));
    }
}
