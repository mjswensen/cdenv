//! Copies optional staged agent artifacts without invoking Cargo.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=CDENV_AGENT_ARTIFACT_DIR");
    println!("cargo:rerun-if-env-changed=CDENV_BUILD_ID");
    let Ok(output) = env::var("OUT_DIR") else {
        return;
    };
    let output = Path::new(&output);
    let source = env::var_os("CDENV_AGENT_ARTIFACT_DIR").map(|path| Path::new(&path).to_path_buf());
    let names = ["cdenv-agent-x86_64", "cdenv-agent-aarch64"];
    let available = source
        .as_ref()
        .is_some_and(|directory| names.iter().all(|name| directory.join(name).is_file()));
    if let Some(directory) = source.as_ref().filter(|_| available) {
        for name in names {
            if let Err(error) = fs::copy(directory.join(name), output.join(name)) {
                println!("cargo:warning=unable to copy staged agent {name}: {error}");
                return write_configuration(output, false);
            }
        }
    } else {
        for name in names {
            if let Err(error) = fs::write(output.join(name), []) {
                println!("cargo:warning=unable to create empty agent placeholder {name}: {error}");
                return write_configuration(output, false);
            }
        }
    }
    write_configuration(output, available);
}

fn write_configuration(output: &Path, staged: bool) {
    if let Err(error) = fs::write(
        output.join("agent_artifacts.rs"),
        format!(
            "pub const STAGED: bool = {staged}; pub const HOST_BUILD_ID: &str = {:?};",
            env::var("CDENV_BUILD_ID").unwrap_or_else(|_| "development".to_owned())
        ),
    ) {
        println!("cargo:warning=unable to write agent artifact configuration: {error}");
    }
}
