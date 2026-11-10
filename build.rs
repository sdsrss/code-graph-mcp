//! Build script: compiles the vendored sqlite-vec C extension and pins it
//! against known-good blake3 hashes (supply-chain integrity for the ~305 KB of
//! vendored C — audit §8). An accidental, malicious, or unreviewed change to the
//! vendored sources fails the build with an actionable message rather than
//! silently shipping altered native code.

/// Upstream sqlite-vec release this vendored copy tracks. Bump together with the
/// pinned hashes below when intentionally updating `vendor/sqlite-vec/`.
const SQLITE_VEC_UPSTREAM_VERSION: &str = "v0.1.9";
/// blake3 of the LINE-NORMALIZED (CR-stripped) vendored sources, so the pin is
/// stable regardless of the checkout's line endings (no .gitattributes needed).
const SQLITE_VEC_C_BLAKE3: &str =
    "0302016b71a04baaa3bf1a34930fcf4e41e02d5f09c0283c92f52d3112fc7760";
const SQLITE_VEC_H_BLAKE3: &str =
    "57b9c859a9f05bd08c48b1ad03c9b7e964269ab2311a31d101bc21974b3a1549";

fn vendor_blake3(path: &str) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    // Strip CR so CRLF and LF checkouts hash identically across platforms.
    let normalized: Vec<u8> = bytes.into_iter().filter(|&b| b != b'\r').collect();
    blake3::hash(&normalized).to_hex().to_string()
}

fn verify_pin(path: &str, expected: &str) {
    let actual = vendor_blake3(path);
    if actual != expected {
        panic!(
            "vendored {path} changed: blake3 {actual} != pinned {expected} \
             (sqlite-vec {SQLITE_VEC_UPSTREAM_VERSION}).\n\
             If this is an intentional upstream update, review the diff, then update \
             the pinned hash (+ SQLITE_VEC_UPSTREAM_VERSION) in build.rs."
        );
    }
}

fn main() {
    println!("cargo:rerun-if-changed=vendor/sqlite-vec/sqlite-vec.c");
    println!("cargo:rerun-if-changed=vendor/sqlite-vec/sqlite-vec.h");
    println!("cargo:rerun-if-changed=build.rs");

    // Supply-chain gate: verify the vendored C before compiling it.
    verify_pin("vendor/sqlite-vec/sqlite-vec.c", SQLITE_VEC_C_BLAKE3);
    verify_pin("vendor/sqlite-vec/sqlite-vec.h", SQLITE_VEC_H_BLAKE3);

    let mut build = cc::Build::new();
    build
        .file("vendor/sqlite-vec/sqlite-vec.c")
        .include("vendor/sqlite-vec")
        .define("SQLITE_CORE", None)
        .warnings(false);

    // Use sqlite3.h from libsqlite3-sys bundled build (set via its `links = "sqlite3"`)
    if let Ok(include_dir) = std::env::var("DEP_SQLITE3_INCLUDE") {
        build.include(include_dir);
    }

    build.compile("sqlite_vec");
}
