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

    // One entry per program: userland/src/bin/NAME.rs, or a directory
    // userland/src/bin/NAME/ with a main.rs (for multi-file programs).
    let mut names: Vec<String> = fs::read_dir(userland.join("src/bin"))
        .expect("userland/src/bin missing")
        .filter_map(|e| {
            let path = e.ok()?.path();
            if path.is_dir() {
                return path.join("main.rs").exists().then(|| path.file_name()?.to_str().map(String::from))?;
            }
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
    let mut c_names = Vec::new();
    for (name, path) in build_c_programs(&root).into_iter().chain(ported_programs(&root)) {
        assert!(!names.contains(&name) && !c_names.contains(&name), "two programs are called {}", name);
        writeln!(out, "    ({:?}, include_bytes!({:?})),", name, path.display().to_string()).unwrap();
        c_names.push(name);
    }
    out.push_str("];\n");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("user_programs.rs"), out).unwrap();

    make_disk_images(&root, &out_dir);
}

/// Format an ext2 image with mke2fs (from e2fsprogs). `populate` fills it
/// from a directory. Options: 1 KiB blocks, and no features the kernel's
/// ext2 driver doesn't keep up to date (directory hashes, resize inode,
/// extended attributes).
fn mke2fs(image: &std::path::Path, size: &str, populate: Option<&std::path::Path>, extra: &[&str]) -> bool {
    let mut cmd = Command::new("mke2fs");
    cmd.args(["-q", "-t", "ext2", "-b", "1024", "-O", "^dir_index,^resize_inode,^ext_attr", "-L", "palladium", "-F"]);
    cmd.args(extra);
    if let Some(dir) = populate {
        cmd.arg("-d").arg(dir);
    }
    cmd.arg(image).arg(size);
    match cmd.status() {
        Ok(s) if s.success() => true,
        _ => {
            println!("cargo:warning=mke2fs failed or is missing (install e2fsprogs); no disk image for {}", image.display());
            false
        }
    }
}

fn make_disk_images(root: &std::path::Path, out_dir: &std::path::Path) {
    let target = root.join("target");
    fs::create_dir_all(&target).unwrap();

    // The disk `cargo run` attaches. Created once: it keeps your files.
    let disk = target.join("disk.img");
    if !disk.exists() {
        mke2fs(&disk, "32M", None, &[]);
    }

    // A scratch disk for the tests (recreated every build).
    let test_disk = target.join("test-disk.img");
    let _ = fs::remove_file(&test_disk);
    mke2fs(&test_disk, "4M", None, &[]);

    // A small prefilled image the ext2 tests load into a RAM disk.
    let content = out_dir.join("ext2-test-content");
    let _ = fs::remove_dir_all(&content);
    fs::create_dir_all(content.join("dir/nested")).unwrap();
    fs::write(content.join("hello.txt"), "Hello from ext2\n").unwrap();
    fs::write(content.join("dir/nested/deep.txt"), "deep\n").unwrap();
    fs::write(content.join("empty"), "").unwrap();
    // 300 KiB: with 1 KiB blocks this needs the double-indirect block.
    let big: Vec<u8> = (0..300 * 1024u32).map(|i| (i % 251) as u8).collect();
    fs::write(content.join("big.bin"), big).unwrap();
    let image = out_dir.join("ext2-test.img");
    let _ = fs::remove_file(&image);
    // More inodes than the default, for the "many files" test.
    if !mke2fs(&image, "2M", Some(&content), &["-N", "1024"]) {
        fs::write(&image, b"").unwrap();
    }
}

/// Compile the C programs in userland-c/ with palladium-cc (gcc + musl in
/// target/musl, made by tools/build-musl.sh): static position-independent
/// executables.
/// Without musl they're skipped (with a warning).
fn build_c_programs(root: &std::path::Path) -> Vec<(String, PathBuf)> {
    let src = root.join("userland-c");
    let musl_gcc = root.join("target/musl/bin/palladium-cc");
    println!("cargo:rerun-if-changed=userland-c");
    println!("cargo:rerun-if-changed=target/musl/lib/libc.a");
    let Ok(entries) = fs::read_dir(&src) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries.filter_map(|e| Some(e.ok()?.path())).filter(|p| p.extension().is_some_and(|x| x == "c")).collect();
    files.sort();
    if files.is_empty() {
        return Vec::new();
    }
    if !musl_gcc.exists() {
        println!("cargo:warning=musl not found: run tools/build-musl.sh to build the C programs in userland-c/");
        return Vec::new();
    }
    let out = root.join("target/userland-c");
    fs::create_dir_all(&out).unwrap();
    let mut built = Vec::new();
    for file in files {
        let name = file.file_stem().unwrap().to_str().unwrap().to_string();
        let exe = out.join(&name);
        let status = Command::new(&musl_gcc)
            .args(["-O2", "-Wall", "-o"])
            .arg(&exe)
            .arg(&file)
            .arg("-lm")
            .status()
            .expect("failed to run palladium-cc");
        assert!(status.success(), "compiling {} failed", file.display());
        built.push((name, exe));
    }
    built
}

/// Ported software built by tools/ports/*.sh (lua, ...), in target/ports/bin.
fn ported_programs(root: &std::path::Path) -> Vec<(String, PathBuf)> {
    let dir = root.join("target/ports/bin");
    println!("cargo:rerun-if-changed=target/ports/bin");
    let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };
    let mut found: Vec<(String, PathBuf)> = entries
        .filter_map(|e| {
            let path = e.ok()?.path();
            let name = path.file_name()?.to_str()?.to_string();
            path.is_file().then_some((name, path))
        })
        .collect();
    found.sort();
    found
}
