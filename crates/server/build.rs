// `sqlx::migrate!` embeds the migrations when the crate compiles; without this, adding a
// migration file would not rebuild the binary, which would then run without it.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
