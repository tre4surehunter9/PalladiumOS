// Copyright (c) 2026 tre4surehunter9
//! Builds the userland (userland/) and bundles every program into the kernel.
//!
//! Each binary in userland/src/bin becomes an entry in USER_PROGRAMS
//! (see src/programs.rs), which `run <name>` looks up.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Load address of user programs: 512 GiB, the second level-4 page-table
/// slot. Slot 0 holds the kernel (0x200000), and each process's user slots
/// must be separate from the kernel's (see memory::is_user_slot).
const USER_IMAGE_BASE: &str = "0x8000000000";

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let userland = root.join("userland");
    let target_json = root.join("x86_64-palladiumos.json");
    let target_dir = root.join("target").join("userland");

    println!("cargo:rerun-if-changed=userland/src");
    println!("cargo:rerun-if-changed=userland/Cargo.toml");
    println!("cargo:rerun-if-changed=x86_64-palladiumos.json");

    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(&userland)
        .args(["build", "--release", "--bins", "--target"])
        .arg(&target_json)
        .arg("--target-dir")
        .arg(&target_dir);

    // Don't leak the outer build's settings into the nested cargo.
    for (key, _) in env::vars() {
        if (key.starts_with("CARGO_") && key != "CARGO_HOME")
            || key.starts_with("__CARGO")
            || key == "RUSTFLAGS"
            || key == "RUSTC_WRAPPER"
            || key == "RUSTC_WORKSPACE_WRAPPER"
        {
            cmd.env_remove(key);
        }
    }
    // Code is position-independent (RIP-relative), which works at any
    // address; static relocations would limit programs to the low 2 GiB.
    // The result is still a static, non-PIE executable at USER_IMAGE_BASE.
    let rustflags = [
        "-Crelocation-model=pic".to_string(),
        "-Clink-arg=--no-pie".to_string(),
        format!("-Clink-arg=--image-base={}", USER_IMAGE_BASE),
    ];
    cmd.env("CARGO_ENCODED_RUSTFLAGS", rustflags.join("\x1f"));

    let status = cmd.status().expect("failed to run cargo for userland");
    if !status.success() {
        panic!("building userland failed");
    }

    // One entry per userland/src/bin/*.rs, sorted by name.
    let mut names: Vec<String> = fs::read_dir(userland.join("src/bin"))
        .expect("userland/src/bin missing")
        .filter_map(|e| {
            let path = e.ok()?.path();
            (path.extension()? == "rs").then(|| path.file_stem()?.to_str().map(String::from))?
        })
        .collect();
    names.sort();

    let bin_dir = target_dir.join("x86_64-palladiumos").join("release");
    let mut out = String::from("pub static USER_PROGRAMS: &[(&str, &[u8])] = &[\n");
    for name in &names {
        let path = bin_dir.join(name);
        assert!(path.exists(), "userland binary {} missing", path.display());
        writeln!(out, "    ({:?}, include_bytes!({:?})),", name, path.display().to_string()).unwrap();
    }
    out.push_str("];\n");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("user_programs.rs"), out).unwrap();
}
