//! Architectural invariants for epic #182:
//!
//! 1. **One-way dependency.** Code under `crate::http::*` MUST NOT import
//!    from `crate::ffi::*`. The FFI bridge wraps the pure-Rust HTTP
//!    layer, never the reverse.
//! 2. **Canonical root layout.** The only entries directly under
//!    `crates/iroh-http-core/src/` are `lib.rs`, `endpoint/`,
//!    `http/`, `ffi/`, and the cross-cutting utility modules extracted
//!    in #198 (`error.rs`, `crypto.rs`, `encoding.rs`, `addr.rs`).
//!    Every other file declares a side. This prevents the kind of
//!    drift that produced the original 979-LoC `server.rs` and
//!    998-LoC `stream.rs`.
//!
//! Slice history (TEMPORARY_EXCEPTIONS evolution):
//!
//! - `http/server/mod.rs` shed FFI imports in Slice C (#185).
//! - `http/client.rs` shed FFI imports in Slice D (#186).
//! - `http/session.rs` moved into `mod ffi` in Slice E (#187) — the
//!   session API is fundamentally `u64`-handle-shaped (`Session` wraps
//!   slotmap entries, returns `FfiDuplexStream`), so it belongs on the
//!   FFI side. With it gone, `TEMPORARY_EXCEPTIONS` is empty and stays
//!   empty — any future slice that needs an exception must extend the
//!   list explicitly and justify it in the diff.
//!
//! No external dev-dep needed — `std::fs` walks the source tree.

use std::{fs, path::Path};

/// Files temporarily exempt from the `mod http` → `mod ffi` ban.
/// **Empty as of Slice E (#187).** Adding an entry here is a structural
/// regression — the reviewer should ask whether the file belongs in
/// `mod ffi` instead.
const TEMPORARY_EXCEPTIONS: &[&str] = &[];

#[test]
fn http_module_does_not_depend_on_ffi() {
    // CARGO_MANIFEST_DIR is the crate root.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let http_dir = Path::new(manifest_dir).join("src").join("http");

    let mut violations: Vec<String> = Vec::new();
    walk(&http_dir, &mut |path| {
        if path.extension().is_some_and(|e| e == "rs") {
            // Compute the path relative to `src/` for allowlist matching.
            let rel = path
                .strip_prefix(Path::new(manifest_dir).join("src"))
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if TEMPORARY_EXCEPTIONS.contains(&rel.as_str()) {
                return;
            }
            let src = fs::read_to_string(path).unwrap_or_default();
            // Strip line comments to avoid false positives in doc/code comments.
            let stripped: String = src
                .lines()
                .map(|line| {
                    if let Some(idx) = line.find("//") {
                        &line[..idx]
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            if stripped.contains("crate::ffi") || stripped.contains("super::ffi") {
                violations.push(format!(
                    "{} imports from crate::ffi — http MUST NOT depend on ffi (epic #182)",
                    path.display()
                ));
            }
        }
    });

    assert!(
        violations.is_empty(),
        "architectural invariant violated:\n{}",
        violations.join("\n")
    );
}

fn walk(dir: &Path, f: &mut impl FnMut(&Path)) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, f);
            } else {
                f(&path);
            }
        }
    }
}

/// Slice E (#187) acceptance #6: assert the canonical root layout.
///
/// Only `lib.rs`, the `http/`, `ffi/`, and `endpoint/` module trees, and the
/// cross-cutting utility modules extracted in #198 (`error.rs`, `crypto.rs`,
/// `encoding.rs`, `addr.rs`) may live directly under
/// `crates/iroh-http-core/src/`. Any other file or folder is a structural
/// drift that the reviewer should have flagged. Adding new top-level modules
/// requires editing both this allowlist *and* the epic acceptance criteria —
/// that intentional friction is the point.
#[test]
fn crate_root_has_canonical_layout() {
    const ALLOWED: &[&str] = &[
        "lib.rs",
        "endpoint",
        "http",
        "ffi",
        "error.rs",
        "crypto.rs",
        "encoding.rs",
        "addr.rs",
    ];

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let src_dir = Path::new(manifest_dir).join("src");

    let mut unexpected: Vec<String> = Vec::new();
    for entry in fs::read_dir(&src_dir).expect("src/ exists").flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !ALLOWED.contains(&name.as_str()) {
            unexpected.push(name);
        }
    }

    assert!(
        unexpected.is_empty(),
        "unexpected entries directly under src/ — only {ALLOWED:?} are allowed (epic #182):\n  {}",
        unexpected.join("\n  ")
    );
}
