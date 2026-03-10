fn main() {
    // Compile sqlite-vec C source as a static library
    cc::Build::new()
        .file("vendor/sqlite-vec/sqlite-vec.c")
        .include("vendor/sqlite-vec")
        .define("SQLITE_CORE", None)
        .warnings(false)
        .compile("sqlite_vec");
}
