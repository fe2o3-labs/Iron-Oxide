//! Rebuilds the server when a migration changes, so `sqlx::migrate!()` embeds the new set.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
