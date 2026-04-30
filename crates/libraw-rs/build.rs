use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=LIBRAW_LIB_DIR");
    println!("cargo:rerun-if-env-changed=LIBRAW_LIB_NAME");
    println!("cargo:rerun-if-env-changed=LIBRAW_INCLUDE_DIR");
    println!("cargo:rerun-if-changed=wrapper.c");

    let include_dir = link_libraw();
    compile_wrapper(include_dir);
}

fn link_libraw() -> Option<PathBuf> {
    if let Some(lib_name) = env::var_os("LIBRAW_LIB_NAME") {
        if let Some(lib_dir) = env::var_os("LIBRAW_LIB_DIR") {
            println!(
                "cargo:rustc-link-search=native={}",
                PathBuf::from(lib_dir).display()
            );
        }
        println!("cargo:rustc-link-lib={}", lib_name.to_string_lossy());
        return env::var_os("LIBRAW_INCLUDE_DIR").map(PathBuf::from);
    }

    if target_os() == "macos"
        && let Some(prefix) = homebrew_prefix("libraw")
    {
        println!("cargo:rustc-link-search=native={}/lib", prefix.display());
        println!("cargo:rustc-link-lib=raw_r");
        return Some(prefix.join("include"));
    }

    if let Some(include) = pkg_config("libraw_r") {
        return include;
    }
    if let Some(include) = pkg_config("libraw") {
        return include;
    }

    panic!(
        "LibRaw was not found. Install LibRaw or set LIBRAW_LIB_DIR and LIBRAW_LIB_NAME, \
         for example LIBRAW_LIB_DIR=/opt/homebrew/lib LIBRAW_LIB_NAME=raw_r"
    );
}

fn compile_wrapper(include_dir: Option<PathBuf>) {
    let mut build = cc::Build::new();
    build.file("wrapper.c");
    if let Some(dir) = include_dir.as_ref() {
        build.include(dir);
    }
    if let Some(env_include) = env::var_os("LIBRAW_INCLUDE_DIR") {
        build.include(PathBuf::from(env_include));
    }
    build.compile("terminalroom_libraw_wrapper");
}

fn target_os() -> String {
    env::var("CARGO_CFG_TARGET_OS").unwrap_or_default()
}

fn pkg_config(package: &str) -> Option<Option<PathBuf>> {
    let libs = Command::new("pkg-config")
        .args(["--libs-only-L", "--libs-only-l", package])
        .output()
        .ok()?;
    if !libs.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&libs.stdout);
    for flag in stdout.split_whitespace() {
        if let Some(path) = flag.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={path}");
        } else if let Some(lib) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={lib}");
        }
    }

    let cflags = Command::new("pkg-config")
        .args(["--cflags-only-I", package])
        .output()
        .ok()?;
    if !cflags.status.success() {
        return Some(None);
    }
    let cflags_out = String::from_utf8_lossy(&cflags.stdout);
    let include = cflags_out
        .split_whitespace()
        .find_map(|f| f.strip_prefix("-I").map(PathBuf::from));
    Some(include)
}

fn homebrew_prefix(formula: &str) -> Option<PathBuf> {
    let output = Command::new("brew")
        .args(["--prefix", formula])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let prefix = String::from_utf8(output.stdout).ok()?;
    let prefix = PathBuf::from(prefix.trim());
    if prefix.as_os_str().is_empty() || !prefix.join("lib").is_dir() {
        None
    } else {
        Some(prefix)
    }
}
