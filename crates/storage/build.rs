fn main() {
    // Stable Rust must rerun the migration macro when a migration is added.
    println!("cargo:rerun-if-changed=migrations");
}
