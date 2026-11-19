# Patch workspaces

A patch workspace lets you keep the changes to a dependency in a small Rust program. Run it when you want to publish an edited version. Projects that use the result depend on the published crate as usual.

## Declare the upstream packages

For example, suppose your project uses `either` and you want a compiler warning when a caller discards an `Either` value. Give the patch program its own workspace and select the upstream version in `patches/Cargo.toml`:

```toml
[package]
name = "patches"
edition = "2024"
publish = false

[workspace]
exclude = ["target/out"]

[dependencies]
override = "0.5"
either = "=1.19.0"
```

## Prepare, edit and write

In `patches/src/main.rs`, add `#[must_use]` to the enum and name the resulting package `checked-either`:

```rust
use std::path::Path;
use r#override::{Package, Result, item};

fn main() -> Result<()> {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/out/checked-either");
    let upstream = Package::load(env!("CARGO_MANIFEST_PATH"))?.dependency("either")?;
    let mut sources = upstream.prepare(out)?;
    sources.edit("src/lib.rs", |source| {
        source.select(item("Either"))?.add_attribute("#[must_use]")
    })?;
    sources.manifest(|manifest| {
        let package = manifest["package"].as_table_mut().unwrap();
        package.insert("name".into(), "checked-either".into());
        package.insert("version".into(), "0.1.0".into());
        let generated = vec![".override-source", "Cargo.toml.orig", ".cargo_vcs_info.json"];
        package.insert("exclude".into(), generated.into());
        Ok(())
    })?;
    Ok(())
}
```

`prepare` starts from the published archive for a registry dependency, the listed files for a vendored dependency, or the files Cargo would package for a Git or path dependency. Choose an empty output directory: each subsequent run recreates that copy from the upstream source.

## Trying the edited crate

From the project root, run `cargo run --manifest-path patches/Cargo.toml`. Point your application's `either` dependency at the result:

```toml
[dependencies]
either = { package = "checked-either", path = "patches/target/out/checked-either" }
```

The application can still import `either::Either`. If a function returns `Either` and its caller discards the result, `cargo check` now reports an unused value.

To publish, run `cargo publish --manifest-path patches/target/out/checked-either/Cargo.toml` and use `version = "0.1.0"` in place of `path` in the application. The example carries the upstream license files into the new package. It excludes the working copy's marker and the two files Cargo generates again during packaging.

## Editing across files

Paths passed to `edit` and `read` are relative to the prepared directory. When an edit needs a declaration in another file, read it and add it as context:

```rust
let declarations = sources.read("src/shared.rs")?;
sources.edit("src/worker.rs", |source| {
    source.set_module("worker")?;
    source.add_source("shared", &declarations);
    source.select(item("Worker::run"))?.set_visibility("pub(crate)")
})?;
```

`edit` saves the file when the closure succeeds. For other resources, `sources.path("config.txt")?` gives the full path. Source files use the package's Rust edition unless you select a target with `sources.target(target)?`. See [Editing Rust](editing.md) for more ways to select and change code.

## Publishing related crates

When another generated crate depends on `either`, change its manifest to use `checked-either`. Keeping the dependency key lets its source keep using `either::`:

```rust
sources.manifest(|manifest| {
    let dependency = manifest["dependencies"]["either"].as_table_mut().unwrap();
    dependency.insert("package".into(), "checked-either".into());
    dependency.insert("version".into(), "=0.1.0".into());
    dependency.insert("path".into(), "../checked-either".into());
    Ok(())
})?;
```

List the generated crates in a workspace manifest under `target/out`, then run `cargo publish --workspace` from that directory. The [parser patch program](https://github.com/zetaloop/override/blob/main/patches/src/main.rs) shows a complete example.

## Using an existing metadata graph

`Package::load` selects a package by its manifest. If you already have a Cargo metadata graph, select it by ID instead. To prepare a registry archive, supply the configuration that tells Cargo where the archive came from:

```rust
use r#override::{Config, Package};

let config = Config::load_with_cwd(project)?;
let package = Package::from_metadata(metadata, &id)?.config(config);
let sources = package.prepare(destination)?;
```

If the original command used `--config` to select a registry mirror, apply those settings to `config` as well. Packages selected through `dependency` share that configuration. When the same dependency name selects different packages on different platforms, pass the relevant declaration from `package.data().dependencies` to `package.resolve(declaration)?`.
