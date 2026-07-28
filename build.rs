//! Сборка Go-моста к typescript-go и линковка его в нативный модуль.
//!
//! Требует Go ≥ 1.26 и сеть на первую сборку — но только у того, кто
//! собирает: в wheel уже лежит готовый статически слинкованный код.
//!
//! Клон typescript-go кладётся рядом с `go/` (в `.ts-go`), а не в OUT_DIR:
//! он весит ~370 МБ и переживает `cargo clean`, иначе каждая чистая
//! сборка тянула бы его заново.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Ревизия typescript-go. Обновление — ручная операция: сменить коммит,
/// пересобрать, сверить результат (см. README).
const TS_GO_REV: &str = "f209df30b6ece1eee7b9264bfd8a68d06fd226c4";
const TS_GO_REPO: &str = "https://github.com/microsoft/typescript-go.git";

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo задаёт её"));
    let bridge = manifest.join("go");
    let checkout = manifest.join(".ts-go");

    println!("cargo:rerun-if-changed=go/bridge.go");
    println!("cargo:rerun-if-changed=go/bridge_go.go");
    println!("cargo:rerun-if-changed=go/go.mod");

    ensure_checkout(&checkout);
    let archive = build_bridge(&bridge, &checkout);

    println!("cargo:rustc-link-search=native={}", archive.display());
    println!("cargo:rustc-link-lib=static=callixbridge");
    // Go-рантайм тянет резолвер имён и pthread.
    println!("cargo:rustc-link-lib=dylib=resolv");
}

fn run(command: &mut Command, what: &str) {
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("не удалось запустить {what}: {e}"));
    assert!(status.success(), "{what} завершился с ошибкой: {status}");
}

/// Клон typescript-go на закреплённой ревизии.
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

/// Собирает мост в c-archive и возвращает каталог с ним.
fn build_bridge(bridge: &Path, checkout: &Path) -> PathBuf {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo задаёт её"));
    let archive = out.join("libcallixbridge.a");

    run(
        Command::new("go")
            .current_dir(bridge)
            // Сборка Go шумит во временный каталог; на маленьком /tmp это
            // упирается в место, поэтому уводим её в OUT_DIR.
            .env("TMPDIR", &out)
            .env("CGO_ENABLED", "1")
            .args(["build", "-buildmode=c-archive", "-o"])
            .arg(&archive)
            .arg("."),
        "go build моста к typescript-go",
    );
    assert!(archive.exists(), "go build не создал {}", archive.display());
    let _ = checkout;
    out
}
