//! Builds the Go bridge to typescript-go and links it into the native
//! module.
//!
//! Requires Go >= 1.26 and network access on the first build — but only for
//! whoever builds: the wheel already carries the statically linked code.
//!
//! The typescript-go clone is placed next to `go/` (in `.ts-go`) rather than
//! in OUT_DIR: it weighs ~370 MB and survives `cargo clean`, or every clean
//! build would fetch it again.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The typescript-go revision. Updating is a manual operation: change the
/// commit, rebuild, and verify the result (see the README).
const TS_GO_REV: &str = "f209df30b6ece1eee7b9264bfd8a68d06fd226c4";
const TS_GO_REPO: &str = "https://github.com/microsoft/typescript-go.git";

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let bridge = manifest.join("go");
    let checkout = manifest.join(".ts-go");

    println!("cargo:rerun-if-changed=go/bridge.go");
    println!("cargo:rerun-if-changed=go/bridge_go.go");
    println!("cargo:rerun-if-changed=go/go.mod");

    ensure_checkout(&checkout);
    let archive = build_bridge(&bridge, &checkout);

    println!("cargo:rustc-link-search=native={}", archive.display());
    println!("cargo:rustc-link-lib=static=callixbridge");
    // The Go runtime pulls in the name resolver and pthread.
    println!("cargo:rustc-link-lib=dylib=resolv");
}

fn run(command: &mut Command, what: &str) {
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("could not run {what}: {e}"));
    assert!(status.success(), "{what} failed: {status}");
}

/// The typescript-go clone at the pinned revision.
fn ensure_checkout(checkout: &Path) {
    if checkout.join(".git").exists() {
        let current = Command::new("git")
            .args(["-C", &checkout.to_string_lossy(), "rev-parse", "HEAD"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();
        if current.trim() == TS_GO_REV {
            return;
        }
    } else {
        run(
            Command::new("git").args([
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                TS_GO_REPO,
                &checkout.to_string_lossy(),
            ]),
            "git clone typescript-go",
        );
    }
    run(
        Command::new("git")
            .args(["-C", &checkout.to_string_lossy()])
            .args(["fetch", "--depth", "1", "origin", TS_GO_REV]),
        "git fetch typescript-go",
    );
    run(
        Command::new("git")
            .args(["-C", &checkout.to_string_lossy()])
            .args(["checkout", "--force", TS_GO_REV]),
        "git checkout typescript-go",
    );
}

/// Builds the bridge into a c-archive and returns its directory.
fn build_bridge(bridge: &Path, checkout: &Path) -> PathBuf {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets it"));
    let archive = out.join("libcallixbridge.a");

    run(
        Command::new("go")
            .current_dir(bridge)
            // The Go build is noisy in the temp directory; on a small /tmp
            // that runs out of space, so it is pointed at OUT_DIR.
            .env("TMPDIR", &out)
            .env("CGO_ENABLED", "1")
            .args(["build", "-buildmode=c-archive", "-o"])
            .arg(&archive)
            .arg("."),
        "go build of the typescript-go bridge",
    );
    assert!(archive.exists(), "go build did not produce {}", archive.display());
    let _ = checkout;
    out
}
