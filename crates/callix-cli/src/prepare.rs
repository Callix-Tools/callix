//! `--prepare`: fetch what the resolvers need to see the project's third-party
//! code, before the analysis starts.
//!
//! Resolvers can only answer for what is on disk: gopls-style loading needs the
//! module cache, rust-analyzer needs the registry sources, and the TypeScript
//! checker needs `node_modules`. In a container that starts empty none of that
//! is there. Python is deliberately left alone — installing a project's
//! packages means running its build backend, which this tool will not do on its
//! own — so for Python mount an environment instead (see the Docker docs).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Directories never searched for manifests.
const SKIP: [&str; 10] = [
    ".git", "node_modules", "target", "vendor", ".venv", "venv", "dist", "build", "__pycache__",
    ".tox",
];

/// How deep below the project root manifests are looked for.
const MAX_DEPTH: usize = 4;

struct Step {
    dir: PathBuf,
    program: &'static str,
    args: &'static [&'static str],
    what: &'static str,
}

fn has_tool(program: &str) -> bool {
    // Go spells it as a subcommand, not a flag.
    let probe = if program == "go" { "version" } else { "--version" };
    Command::new(program)
        .arg(probe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn walk(dir: &Path, depth: usize, steps: &mut Vec<Step>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut subdirs = Vec::new();
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        match entry.file_type() {
            Ok(t) if t.is_dir() => {
                if !SKIP.contains(&name.as_str()) {
                    subdirs.push(entry.path());
                }
            }
            Ok(t) if t.is_file() => files.push(name),
            _ => {}
        }
    }
    let has = |name: &str| files.iter().any(|f| f == name);

    if has("go.mod") {
        steps.push(Step { dir: dir.to_owned(), program: "go", args: &["mod", "download"], what: "Go modules" });
    }
    if has("Cargo.toml") {
        steps.push(Step { dir: dir.to_owned(), program: "cargo", args: &["fetch", "--quiet"], what: "Cargo crates" });
    }
    if has("package.json") && !dir.join("node_modules").exists() {
        let step = if has("pnpm-lock.yaml") {
            Step { dir: dir.to_owned(), program: "pnpm", args: &["install", "--frozen-lockfile", "--ignore-scripts"], what: "pnpm packages" }
        } else if has("yarn.lock") {
            Step { dir: dir.to_owned(), program: "yarn", args: &["install", "--frozen-lockfile", "--ignore-scripts"], what: "Yarn packages" }
        } else if has("package-lock.json") {
            Step { dir: dir.to_owned(), program: "npm", args: &["ci", "--ignore-scripts", "--no-audit", "--no-fund"], what: "npm packages" }
        } else {
            Step { dir: dir.to_owned(), program: "npm", args: &["install", "--ignore-scripts", "--no-audit", "--no-fund"], what: "npm packages" }
        };
        steps.push(step);
    }

    if depth < MAX_DEPTH {
        subdirs.sort();
        for sub in subdirs {
            walk(&sub, depth + 1, steps);
        }
    }
}

/// Runs every applicable step under `root` and returns the problems met.
/// A step that fails only costs resolution quality, never the analysis.
pub fn run(root: &Path, quiet: bool) -> Vec<String> {
    let mut steps = Vec::new();
    walk(root, 0, &mut steps);

    let mut problems = Vec::new();
    for step in steps {
        let rel = step.dir.strip_prefix(root).unwrap_or(&step.dir);
        let shown = if rel.as_os_str().is_empty() { ".".into() } else { rel.display().to_string() };
        if !has_tool(step.program) {
            problems.push(format!("{}: {} is not installed; {} not fetched", shown, step.program, step.what));
            continue;
        }
        if !quiet {
            eprintln!("preparing {} in {shown}: {} {}", step.what, step.program, step.args.join(" "));
        }
        let status = Command::new(step.program)
            .args(step.args)
            .current_dir(&step.dir)
            .stdout(Stdio::null())
            .stderr(if quiet { Stdio::null() } else { Stdio::inherit() })
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => problems.push(format!("{shown}: `{} {}` exited with {s}", step.program, step.args.join(" "))),
            Err(e) => problems.push(format!("{shown}: cannot run {}: {e}", step.program)),
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str, files: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("callix-prepare-{}-{name}", std::process::id()));
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        root
    }

    fn steps(root: &Path) -> Vec<(String, &'static str)> {
        let mut found = Vec::new();
        walk(root, 0, &mut found);
        found
            .into_iter()
            .map(|s| (s.dir.strip_prefix(root).unwrap().display().to_string(), s.program))
            .collect()
    }

    #[test]
    fn finds_manifests_at_every_level() {
        let root = tree("levels", &["go.mod", "svc/Cargo.toml", "web/package.json", "web/package-lock.json"]);
        let found = steps(&root);
        assert!(found.contains(&("".into(), "go")));
        assert!(found.contains(&("svc".into(), "cargo")));
        assert!(found.contains(&("web".into(), "npm")));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn skips_vendored_and_generated_trees() {
        let root = tree(
            "skip",
            &["node_modules/x/package.json", "vendor/m/go.mod", "target/Cargo.toml", ".venv/lib/go.mod"],
        );
        assert!(steps(&root).is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn installed_packages_are_left_alone() {
        let root = tree("installed", &["package.json", "node_modules/a/index.js"]);
        assert!(steps(&root).is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_package_manager_follows_the_lockfile() {
        let pnpm = tree("pnpm", &["package.json", "pnpm-lock.yaml"]);
        assert_eq!(steps(&pnpm), [("".to_owned(), "pnpm")]);
        let yarn = tree("yarn", &["package.json", "yarn.lock"]);
        assert_eq!(steps(&yarn), [("".to_owned(), "yarn")]);
        let none = tree("nolock", &["package.json"]);
        assert_eq!(steps(&none), [("".to_owned(), "npm")]);
        for dir in [pnpm, yarn, none] {
            fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn does_not_descend_past_the_depth_limit() {
        let root = tree("deep", &["a/b/c/d/e/go.mod", "a/go.mod"]);
        let found = steps(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "a");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_missing_tool_is_a_problem_not_a_crash() {
        let root = tree("missing", &["go.mod"]);
        // Whatever is installed here, a step for an absent program is reported.
        assert!(!has_tool("definitely-not-a-real-program-callix"));
        fs::remove_dir_all(root).ok();
    }
}
