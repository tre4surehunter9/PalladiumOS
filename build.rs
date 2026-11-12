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

/// Load address of PalladiumOS's own (Rust) programs: 512 GiB. Any lower-
/// half address works (the kernel is in the higher half); this one stays
/// clear of where Linux programs and the brk heap go.
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
    for (name, path) in build_c_programs(&root).into_iter().chain(build_dynamic(&root)).chain(scripts(&root)).chain(ported_programs(&root)) {
        assert!(!names.contains(&name) && !c_names.contains(&name), "two programs are called {}", name);
        writeln!(out, "    ({:?}, include_bytes!({:?})),", name, path.display().to_string()).unwrap();
        c_names.push(name);
    }
    out.push_str("];\n");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("user_programs.rs"), out).unwrap();

    make_disk_images(&root, &out_dir);
    build_runner(&root);
}

/// tools/runner, the program cargo runs kernels with (it makes the bootable
/// disk image and starts QEMU). A host program: built from outside the
/// repository, so this repository's cargo settings (the kernel's target,
/// build-std) don't apply to it, with the nightly toolchain the bootloader
/// crate needs.
fn build_runner(root: &std::path::Path) {
    println!("cargo:rerun-if-changed=tools/runner/src");
    println!("cargo:rerun-if-changed=tools/runner/Cargo.toml");
    let mut cmd = Command::new("cargo");
    cmd.current_dir(env::temp_dir())
        .args(["+nightly", "build", "--release", "--manifest-path"])
        .arg(root.join("tools/runner/Cargo.toml"))
        .arg("--target-dir")
        .arg(root.join("target/runner"));
    for (key, _) in env::vars() {
        if key.starts_with("CARGO") || key.starts_with("RUSTUP_TOOLCHAIN") || key == "RUSTFLAGS" || key == "RUST_TARGET_PATH" || key.starts_with("RUSTC") {
            cmd.env_remove(key);
        }
    }
    let status = cmd.status().expect("failed to run cargo for tools/runner");
    assert!(status.success(), "building tools/runner failed");
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
    // Room for Alpine Linux (/alpine, see userland-sh/alpine): empty, 1 GiB
    // (sparse: it only takes space on this machine as it fills). Created once.
    let alpine = target.join("alpine.img");
    if !alpine.exists() {
        mke2fs(&alpine, "1G", None, &[]);
    }

    build_linux_test(root);
    make_system_disk(root, &target);

    // A scratch disk for the tests (recreated every build).
    let test_disk = target.join("test-disk.img");
    let _ = fs::remove_file(&test_disk);
    mke2fs(&test_disk, "4M", None, &[]);
    // And a bigger one (sparse), for files larger than memory filesystems
    // allow: the usermode tests mount it at /alpine.
    let scratch = target.join("test-scratch.img");
    let _ = fs::remove_file(&scratch);
    mke2fs(&scratch, "64M", None, &[]);

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
    // (C++ programs too, NAME.cpp, if tools/ports/cxx.sh has made
    // palladium-c++.)
    let cxx = root.join("target/musl/bin/palladium-c++");
    println!("cargo:rerun-if-changed=target/musl/lib/libstdc++.a");
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "c" || (x == "cpp" && cxx.exists())))
        .collect();
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
        let is_cxx = file.extension().is_some_and(|x| x == "cpp");
        // (C++ programs are stripped: libstdc++ makes them big, and /bin is
        // in the kernel's memory.)
        let status = Command::new(if is_cxx { &cxx } else { &musl_gcc })
            .args(if is_cxx { &["-std=c++23", "-O2", "-Wall", "-s", "-o"][..] } else { &["-O2", "-Wall", "-o"][..] })
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

/// Dynamic linking: musl's libc.so (also its dynamic linker; PalladiumOS
/// makes /lib/ld-musl-x86_64.so.1 point at /usr/lib/libc.so) goes on the
/// system disk, with `ldd` (musl's libc.so answers to that name) and the
/// shared libraries in userland-c/dynamic/lib*.c. The other programs there
/// are linked dynamically (against them) and go in /bin.
fn build_dynamic(root: &std::path::Path) -> Vec<(String, PathBuf)> {
    let src = root.join("userland-c/dynamic");
    let cc = root.join("target/musl/bin/palladium-cc-dyn");
    let libc_so = root.join("target/musl/lib/libc.so");
    println!("cargo:rerun-if-changed=userland-c/dynamic");
    println!("cargo:rerun-if-changed=target/musl/lib/libc.so");
    if !cc.exists() || !libc_so.exists() {
        return Vec::new(); // musl built without shared libraries
    }
    let lib = root.join("target/sysroot/lib");
    let bin = root.join("target/sysroot/bin");
    fs::create_dir_all(&lib).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::copy(&libc_so, lib.join("libc.so")).unwrap();
    let ldd = bin.join("ldd");
    let _ = fs::remove_file(&ldd);
    std::os::unix::fs::symlink("/usr/lib/libc.so", &ldd).unwrap();

    let Ok(entries) = fs::read_dir(&src) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries.filter_map(|e| Some(e.ok()?.path())).filter(|p| p.extension().is_some_and(|x| x == "c")).collect();
    files.sort();
    let (libs, programs): (Vec<PathBuf>, Vec<PathBuf>) =
        files.into_iter().partition(|p| p.file_name().unwrap().to_str().unwrap().starts_with("lib"));
    let compile = |args: &[&std::ffi::OsStr]| {
        let status = Command::new(&cc).args(["-O2", "-Wall"]).args(args).status().expect("failed to run palladium-cc-dyn");
        assert!(status.success(), "compiling {:?} failed", args);
    };
    let mut link_libs = Vec::new();
    for file in &libs {
        let name = file.file_stem().unwrap().to_str().unwrap().to_string();
        let so = lib.join(format!("{}.so", name));
        compile(&["-shared".as_ref(), "-fPIC".as_ref(), "-o".as_ref(), so.as_os_str(), file.as_os_str()]);
        link_libs.push(format!("-l{}", name.trim_start_matches("lib")));
    }
    let out = root.join("target/userland-c");
    fs::create_dir_all(&out).unwrap();
    let lflag = format!("-L{}", lib.display());
    let mut built = Vec::new();
    for file in &programs {
        let name = file.file_stem().unwrap().to_str().unwrap().to_string();
        let exe = out.join(&name);
        let mut args: Vec<&std::ffi::OsStr> = vec!["-o".as_ref(), exe.as_os_str(), file.as_os_str(), lflag.as_ref()];
        args.extend(link_libs.iter().map(|l| <String as AsRef<std::ffi::OsStr>>::as_ref(l)));
        compile(&args);
        built.push((name, exe));
    }
    built
}

/// Shell scripts in userland-sh/ (like `alpine`), installed in /bin as they are.
fn scripts(root: &std::path::Path) -> Vec<(String, PathBuf)> {
    println!("cargo:rerun-if-changed=userland-sh");
    let Ok(entries) = fs::read_dir(root.join("userland-sh")) else { return Vec::new() };
    let mut found: Vec<(String, PathBuf)> = entries
        .filter_map(|e| {
            let path = e.ok()?.path();
            Some((path.file_name()?.to_str()?.to_string(), path.clone())).filter(|_| path.is_file())
        })
        .collect();
    found.sort();
    found
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

/// The system disk, mounted read-only at /usr: target/sysroot (C headers
/// and libraries, staged by tools/ports/tcc.sh) as an ext2 image, made
/// fresh every build so it never mixes with your files on disk.img. An
/// empty image when there's no sysroot (QEMU always attaches it).
fn make_system_disk(root: &std::path::Path, target: &std::path::Path) {
    let sysroot = root.join("target/sysroot");
    println!("cargo:rerun-if-changed=target/sysroot");
    let image = target.join("usr.img");
    let _ = fs::remove_file(&image);
    let empty = target.join("usr-empty");
    let source = if sysroot.is_dir() {
        sysroot.clone()
    } else {
        fs::create_dir_all(&empty).unwrap();
        empty
    };
    // Room for the files plus some slack (sizes in KiB).
    let used = dir_size(&source) / 1024;
    let size = format!("{}K", (used * 3 / 2 + 2048).max(4096));
    if !mke2fs(&image, &size, Some(&source), &[]) {
        return;
    }
    // The files belong to root, not to whoever ran the build (mke2fs -d
    // copies the host owners; uid 1000 is "user" in PalladiumOS).
    let mut cmds = String::new();
    for path in walk(&source) {
        let rel = path.strip_prefix(&source).unwrap();
        let p = format!("/{}", rel.display());
        writeln!(cmds, "sif \"{}\" uid 0\nsif \"{}\" gid 0", p, p).unwrap();
    }
    cmds.push_str("sif / uid 0\nsif / gid 0\n");
    let cmd_file = target.join("usr-img.debugfs");
    fs::write(&cmd_file, cmds).unwrap();
    let ok = Command::new("debugfs").arg("-w").arg("-f").arg(&cmd_file).arg(&image).output().is_ok_and(|o| o.status.success());
    if !ok {
        println!("cargo:warning=debugfs failed: files on the system disk keep their host owners");
    }
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                out.extend(walk(&path));
            }
            out.push(path);
        }
    }
    out
}

fn dir_size(dir: &std::path::Path) -> u64 {
    walk(dir).iter().filter_map(|p| p.metadata().ok()).map(|m| if m.is_file() { m.len() + 1024 } else { 1024 }).sum()
}

/// A plain Linux program (tests/linux/hello.c) built by the host's gcc,
/// statically against glibc, at the usual Linux address: it goes on the
/// system disk as /usr/share/linux-test/hello, to check that PalladiumOS
/// runs Linux binaries unchanged. Skipped if the host can't link
/// statically (no static glibc).
fn build_linux_test(root: &std::path::Path) {
    let src = root.join("tests/linux/hello.c");
    println!("cargo:rerun-if-changed=tests/linux/hello.c");
    let sysroot = root.join("target/sysroot");
    let out_dir = sysroot.join("share/linux-test");
    if !sysroot.is_dir() {
        return; // no system disk contents (tcc not built): nothing to add to
    }
    fs::create_dir_all(&out_dir).unwrap();
    let ok = Command::new("gcc")
        .args(["-static", "-O2", "-o"])
        .arg(out_dir.join("hello"))
        .arg(&src)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        let _ = fs::remove_dir_all(&out_dir);
        println!("cargo:warning=host gcc can't link statically: skipping the Linux binary test");
        return;
    }
    // The same, dynamically linked (as most Linux programs are), with the
    // host's glibc dynamic linker and libc.so.6 on the system disk.
    // PalladiumOS makes /lib64/ld-linux-x86-64.so.2 point at
    // /usr/lib/ld-linux-x86-64.so.2.
    let ld = std::path::Path::new("/lib64/ld-linux-x86-64.so.2");
    let libc6 = Command::new("gcc").arg("-print-file-name=libc.so.6").output().ok()
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string()));
    let dyn_ok = ld.exists()
        && libc6.as_ref().is_some_and(|p| p.is_absolute() && p.exists())
        && Command::new("gcc")
            .args(["-O2", "-o"])
            .arg(out_dir.join("hello-dyn"))
            .arg(&src)
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .status()
            .is_ok_and(|s| s.success());
    if dyn_ok {
        let lib = sysroot.join("lib");
        fs::create_dir_all(&lib).unwrap();
        fs::copy(ld, lib.join("ld-linux-x86-64.so.2")).unwrap();
        fs::copy(libc6.unwrap(), lib.join("libc.so.6")).unwrap();
    } else {
        let _ = fs::remove_file(out_dir.join("hello-dyn"));
    }
}
