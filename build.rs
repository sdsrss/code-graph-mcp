fn main() {
    println!("cargo:rerun-if-changed=vendor/sqlite-vec/sqlite-vec.c");
    println!("cargo:rerun-if-changed=vendor/sqlite-vec/sqlite-vec.h");

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
