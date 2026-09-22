#![cfg(feature = "build")]

use std::{fs, process::Command};

#[test]
fn bridge() -> r#override::Result<()> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path();
    let library = env!("CARGO_MANIFEST_DIR");
    fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"[workspace]
resolver = "3"
members = ["upstream", "dependency", "replacement", "support", "bridge"]

[workspace.package]
edition = "2024"

[workspace.dependencies]
override = {{ path = {library:?} }}

[workspace.lints.rust]
unused_must_use = "deny"
"#
        ),
    )?;
    for (file, contents) in [
        (
            "dependency/Cargo.toml",
            "[package]\nname = \"dependency\"\nversion = \"1.0.0\"\nedition.workspace = true\n",
        ),
        ("dependency/src/lib.rs", "pub fn value() -> i32 { 4 }"),
        (
            "replacement/Cargo.toml",
            "[package]\nname = \"replacement\"\nversion = \"0.1.0\"\nedition.workspace = true\n",
        ),
        ("replacement/src/lib.rs", "pub fn value() -> i32 { 5 }"),
        (
            "upstream/Cargo.toml",
            r#"[package]
name = "upstream"
version = "1.0.0"
edition.workspace = true

[dependencies]
dependency = { path = "../dependency", version = "1" }

[features]
default = ["active"]
active = []

[lints]
workspace = true
"#,
        ),
        (
            "upstream/src/lib.rs",
            "#![no_std]\nmod inner;\nconst SCALE: i32 = 2;\npub fn value() -> i32 { dependency::value() + SCALE * inner::OFFSET }",
        ),
        ("upstream/src/inner.rs", "pub const OFFSET: i32 = 10;"),
        (
            "upstream/build.rs",
            r#"fn main() {
    println!("cargo::rustc-env=SCRIPT_PACKAGE={}", env!("CARGO_PKG_NAME"));
    println!("cargo::rustc-env=SCRIPT_FEATURE={}", cfg!(feature = "active"));
    println!("cargo::rustc-env=SCRIPT_RESOURCE={}", include_str!("config.txt").trim());
}"#,
        ),
        ("upstream/config.txt", "copied resource\n"),
        (
            "support/Cargo.toml",
            r#"[package]
name = "support"
version = "0.1.0"
edition.workspace = true

[build-dependencies]
override.workspace = true

[target.'cfg(any())'.dependencies]
upstream = { path = "../upstream", version = "=1.0.0" }
"#,
        ),
        (
            "support/src/lib.rs",
            "include!(concat!(env!(\"OUT_DIR\"), \"/script.rs\"));",
        ),
        (
            "support/build.rs",
            r#"use r#override::{Package, item};

fn main() -> r#override::Result<()> {
    let package = Package::current()?;
    let upstream = package.dependency("upstream")?;
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let mut sources = upstream.prepare(out.join("source"))?;
    sources.edit("build.rs", |file| file.select(item("main"))?.set_visibility("pub"))?;
    sources.include(upstream.build_script()?)?.export("include_build", out.join("script.rs"))?;
    Ok(())
}"#,
        ),
        (
            "bridge/Cargo.toml",
            r#"[package]
name = "bridge"
version = "0.1.0"
edition.workspace = true

[dependencies]
dependency = { package = "replacement", path = "../replacement" }

[build-dependencies]
override.workspace = true
support = { path = "../support" }

[target.'cfg(any())'.dependencies]
upstream = { path = "../upstream", version = "=1.0.0" }

[features]
default = ["active"]
active = []

[lints]
workspace = true
"#,
        ),
        (
            "bridge/build.rs",
            r#"use r#override::{DependencyKind, Package, check_target, item};

mod script { support::include_build!(); }

fn main() -> r#override::Result<()> {
    script::main();
    let bridge = Package::current()?;
    let upstream = bridge.dependency("upstream")?;
    let replacements = [("dependency", bridge.dependency("dependency")?)];
    bridge.check_dependencies(&upstream, DependencyKind::Normal, &replacements)?;
    bridge.check_features(&upstream, &replacements)?;
    bridge.check_lints(&upstream)?;
    check_target(upstream.library()?, bridge.library()?)?;
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let mut sources = upstream.prepare(out.join("source"))?;
    sources.edit("src/lib.rs", |file| file.select(item("SCALE"))?.set_value("3"))?;
    sources.include(upstream.library()?)?.emit("UPSTREAM_SOURCE")?;
    Ok(())
}"#,
        ),
        (
            "bridge/src/lib.rs",
            "#![no_std]\ninclude!(env!(\"UPSTREAM_SOURCE\"));",
        ),
        (
            "bridge/src/main.rs",
            r#"fn main() {
    assert_eq!(bridge::value(), 35);
    assert_eq!(env!("SCRIPT_PACKAGE"), "bridge");
    assert_eq!(env!("SCRIPT_FEATURE"), "true");
    assert_eq!(env!("SCRIPT_RESOURCE"), std::env::args().nth(1).unwrap());
}"#,
        ),
    ] {
        let path = root.join(file);
        fs::create_dir_all(path.parent().ok_or("file has no parent")?)?;
        fs::write(path, contents)?;
    }
    for resource in ["copied resource", "updated resource"] {
        fs::write(root.join("upstream/config.txt"), resource)?;
        let output = Command::new(env!("CARGO"))
            .current_dir(root)
            .args(["run", "--quiet", "--package", "bridge", "--", resource])
            .env("CARGO_TARGET_DIR", root.join("target"))
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "bridge build failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
    }
    Ok(())
}
