use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::{env, fs};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for name in [
        "PROFILE",
        "OPT_LEVEL",
        "DEBUG",
        "TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_CFG_TARGET_FEATURE",
    ] {
        let value = env::var(name).unwrap_or_default().replace('\u{1f}', " ");
        println!("cargo:rustc-env=KRAAI_PERF_{name}={value}");
    }
    if env::var("CARGO_CFG_TARGET_OS")? == "linux"
        && env::var("PROFILE")? == "release"
        && env::var("DEBUG")? == "true"
    {
        println!("cargo:rustc-link-arg-bin=kraai-perf=-Wl,--no-rosegment");
    }
    println!("cargo:rerun-if-changed=build.rs");
    let profile_manifest = Path::new("../../Cargo.toml");
    println!("cargo:rerun-if-changed={}", profile_manifest.display());
    let profiles = match fs::read_to_string(profile_manifest) {
        Ok(source) => {
            let manifest: toml::Value = toml::from_str(&source)?;
            serde_json::to_string(&manifest.get("profile"))?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::from("unavailable"),
        Err(error) => return Err(error.into()),
    };
    println!("cargo:rustc-env=KRAAI_PERF_PROFILES={profiles}");
    let overrides: BTreeMap<_, _> = env::vars()
        .filter(|(name, _)| {
            name.starts_with("CARGO_PROFILE_") || name.starts_with("CARGO_FEATURE_")
        })
        .collect();
    for name in overrides
        .keys()
        .filter(|name| name.starts_with("CARGO_PROFILE_"))
    {
        println!("cargo:rerun-if-env-changed={name}");
    }
    println!(
        "cargo:rustc-env=KRAAI_PERF_BUILD_OVERRIDES={}",
        serde_json::to_string(&overrides)?
    );
    let output = Command::new(env::var_os("RUSTC").ok_or("RUSTC missing")?)
        .arg("--version")
        .output()?;
    if !output.status.success() {
        return Err("could not identify Rust compiler".into());
    }
    println!(
        "cargo:rustc-env=KRAAI_PERF_RUSTC={}",
        String::from_utf8(output.stdout)?.trim()
    );
    Ok(())
}
