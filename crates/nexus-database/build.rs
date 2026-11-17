fn main() {
    // Re-embed migrations whenever a migration file changes.
    println!("cargo:rerun-if-changed=../../migrations");
}
