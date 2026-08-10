//! Verifies the vendored profile schema before compiling the library.

use std::env;
use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

const EXPECTED: &str = "a0883c0405ff433db188849d458fb20b9c0d73e0ba1a6e44c1d83f3b485408dd";

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let schema = manifest.join("vendor/devContainer.base.schema.json");
    println!("cargo::rerun-if-changed={}", schema.display());
    let bytes = fs::read(&schema).unwrap_or_else(|error| {
        panic!(
            "cannot read pinned Dev Container schema {}: {error}",
            schema.display()
        )
    });
    let actual = hex::encode(Sha256::digest(bytes));
    assert_eq!(
        actual, EXPECTED,
        "vendored Dev Container schema does not match the accepted ADR checksum"
    );
}
