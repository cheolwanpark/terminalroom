use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=LIBRAW_LIB_DIR");
    println!("cargo:rerun-if-env-changed=LIBRAW_LIB_NAME");

    if let Some(lib_name) = env::var_os("LIBRAW_LIB_NAME") {
        if let Some(lib_dir) = env::var_os("LIBRAW_LIB_DIR") {
            println!(
                "cargo:rustc-link-search=native={}",
                PathBuf::from(lib_dir).display()
            );
        }
        println!("cargo:rustc-link-lib={}", lib_name.to_string_lossy());
        return;
    }

    if target_os() == "macos"
        && let Some(prefix) = homebrew_prefix("libraw")
    {
        println!("cargo:rustc-link-search=native={}/lib", prefix.display());
        println!("cargo:rustc-link-lib=raw_r");
        return;
    }

    if pkg_config("libraw_r") || pkg_config("libraw") {
        return;
    }

    panic!(
        "LibRaw was not found. Install LibRaw or set LIBRAW_LIB_DIR and LIBRAW_LIB_NAME, \
         for example LIBRAW_LIB_DIR=/opt/homebrew/lib LIBRAW_LIB_NAME=raw_r"
    );
}

fn target_os() -> String {
    env::var("CARGO_CFG_TARGET_OS").unwrap_or_default()
}

fn pkg_config(package: &str) -> bool {
    let output = Command::new("pkg-config")
        .args(["--libs-only-L", "--libs-only-l", package])
        .output();

    let Ok(output) = output else {
        return false;
    };

    if !output.status.success() {
        return false;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for flag in stdout.split_whitespace() {
        if let Some(path) = flag.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={path}");
        } else if let Some(lib) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={lib}");
        }
    }

    true
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
