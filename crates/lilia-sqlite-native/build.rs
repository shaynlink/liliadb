fn main() {
    println!("cargo:rerun-if-changed=sqlite3/sqlite3.c");
    println!("cargo:rerun-if-changed=sqlite3/sqlite3.h");

    let mut build = cc::Build::new();
    build
        .file("sqlite3/sqlite3.c")
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
        .flag("-DSQLITE_USE_URI=1")
        .flag("-DHAVE_USLEEP=1")
        .flag("-DHAVE_ISNAN=1")
        .warnings(false)
        .compile("lilia_sqlite3");
}
