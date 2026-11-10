fn main() {
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if target_arch == "wasm32" {
        return;
    }

    println!("cargo:rerun-if-changed=sqlite3/sqlite3.c");
    println!("cargo:rerun-if-changed=sqlite3/sqlite3.h");
    println!("cargo:rerun-if-changed=sqlite3/sqlite3ext.h");
    println!("cargo:rerun-if-changed=sqlite3/ext/series.c");
    println!("cargo:rerun-if-changed=sqlite3/ext/decimal.c");

    let mut build = cc::Build::new();
    build
        .file("sqlite3/sqlite3.c")
        .file("sqlite3/ext/series.c")
        .file("sqlite3/ext/decimal.c")
        .include("sqlite3")
        .opt_level(3)
        .flag("-DSQLITE_CORE")
        .flag("-DSQLITE_DEFAULT_MEMSTATUS=0")
        .flag("-DSQLITE_THREADSAFE=2")
        .flag("-DSQLITE_DQS=0")
        .flag("-DSQLITE_LIKE_DOESNT_MATCH_BLOBS")
        .flag("-DSQLITE_OMIT_DEPRECATED")
        .flag("-DSQLITE_DEFAULT_FOREIGN_KEYS=1")
        .flag("-DSQLITE_ENABLE_COLUMN_METADATA")
        .flag("-DSQLITE_ENABLE_JSON1")
        .flag("-DSQLITE_ENABLE_FTS5")
        .flag("-DSQLITE_ENABLE_RTREE")
        .flag("-DSQLITE_ENABLE_GEOPOLY")
        .flag("-DSQLITE_ENABLE_CARRAY")
        .flag("-DSQLITE_ENABLE_SESSION")
        .flag("-DSQLITE_ENABLE_PREUPDATE_HOOK")
        .flag("-DSQLITE_ENABLE_DBSTAT_VTAB")
        .flag("-DSQLITE_ENABLE_BYTECODE_VTAB")
        .flag("-DSQLITE_USE_URI=1")
        .flag("-DHAVE_USLEEP=1")
        .flag("-DHAVE_ISNAN=1")
        .warnings(false)
        .compile("lilia_sqlite3");
}
