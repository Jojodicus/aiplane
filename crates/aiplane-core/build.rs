// `sqlx::migrate!` embeds the migration files at compile time but does not tell
// cargo it read them, so a new migration would otherwise leave this crate (and
// every test binary that embeds the set) on the stale schema.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
