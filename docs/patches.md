# Patch workspaces

A patch workspace edits upstream packages and writes them out as complete crates for publishing. The project then depends on the published crates like any other dependency.

`Package`, `Sources` and `Manifest` come from the default `build` feature.

## Declare the upstream packages

The workspace sits in a directory of the repository, such as `patches/`, excluded from the root workspace:

```toml
[workspace]
exclude = ["patches"]
```

Its manifest lists the upstream packages as normal dependencies:

```toml
[package]
name = "patches"
edition = "2024"
publish = false

[dependencies]
override = "0.4"
upstream = { package = "ra_ap_ide_assists", version = "=0.0.352" }
```

## Prepare, edit and write

Its program copies each upstream package into a directory under `target/` and edits it there:

```rust
use std::path::Path;
use r#override::{Package, Result};

fn main() -> Result<()> {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/out");
    let upstream = Package::load(env!("CARGO_MANIFEST_PATH"))?.dependency("upstream")?;
    let mut sources = upstream.prepare(out.join("assists"))?;
    sources.manifest(|manifest| {
        manifest["package"]["name"] = "project-assists".into();
        manifest["package"]["version"] = "0.1.0".into();
        Ok(())
    })?;
    Ok(())
}
```

[Editing Rust](editing.md) describes the edits that go between `prepare` and writing the manifest.

### Source contents

`prepare` takes the files from wherever Cargo got the package:

| Source | Prepared input |
| :--- | :--- |
| Registry or registry mirror | The corresponding published `.crate` archive |
| Local registry | The `.crate` archive in the configured local registry |
| Vendor directory | Files listed in `.cargo-checksum.json` |
| Git or path dependency | Actual file paths from Cargo's package file listing |

Registry packages give exactly the published contents. Git and path packages give the files Cargo would package under its include and exclude rules, including the original manifest and resources outside the package directory. For vendored packages, a listed file that is missing is an error.

Listing Git and path files uses Cargo's structured file list. That option is unstable, so override sets `RUSTC_BOOTSTRAP=1` to use it with stable Cargo.

The output directory must be empty or come from an earlier `prepare` call. Each call rebuilds it from the upstream files, so edits always start from the original source.

Preparing into two directories gives two copies that can be edited differently:

```rust
let host = upstream.prepare(out.join("host"))?;
let guest = upstream.prepare(out.join("guest"))?;
```

### Working with files

`Sources::directory()` returns the prepared directory, and `package()` returns the package it came from. File paths are relative to that directory:

```rust
sources.edit("src/lib.rs", |source| {
    source.select(r#override::item("SCALE"))?.set_value("3")
})?;
```

Queries see the declarations of that upstream version.

When a file refers to declarations in another file, read that file and add it as context:

```rust
let declarations = sources.read("src/shared.rs")?;
sources.edit("src/worker.rs", |source| {
    source.set_module("worker")?;
    source.add_source("shared", &declarations);
    source.select(item("Worker::run"))?.set_visibility("pub(crate)")
})?;
```

`read` parses a file without changing it, and `edit` saves the file only if the closure succeeds. `path` gives the full path for other resources:

```rust
let config = sources.path("config.txt")?;
```

Files are parsed with the package's edition; `sources.target(target)?` switches to a particular target's edition.

### Editing the manifest

`Sources::manifest` edits the prepared `Cargo.toml`. `Manifest` is a re-export of `toml::Table`.

When one generated crate depends on another, the dependency keeps its name and points to the new package:

```rust
sources.manifest(|manifest| {
    let dependency = manifest["dependencies"]["ide-db"].as_table_mut().unwrap();
    dependency.insert("package".into(), "project-ide-db".into());
    dependency.insert("version".into(), "=0.1.0".into());
    dependency.insert("path".into(), "../project-ide-db".into());
    Ok(())
})?;
```

Registry archives also contain `Cargo.toml.orig` and `.cargo_vcs_info.json`, which `cargo package` creates again, so remove them before publishing. The `.override-source` marker belongs in the package's `exclude` list.

## Selecting packages and targets

`Package::load(manifest)` reads Cargo metadata for a manifest with all features enabled. `Package::current()` uses the `CARGO_MANIFEST_PATH` that Cargo sets for running programs and build scripts.

`Package::data()` returns Cargo's information about the package, and `directory()` and `edition()` return its source directory and Rust edition.

```rust
let upstream = patches.dependency("upstream")?;
let library = upstream.library()?;
let script = upstream.build_script()?;
let binary = upstream.target(r#override::TargetKind::Bin, "tool")?;
```

`library()` requires exactly one library target; `target(kind, name)` selects other targets by name.

A dependency name may be declared several times for different platforms, each selecting a different package. In that case, pick the declaration from `patches.data().dependencies` and pass it to `patches.resolve(declaration)?`. Each declaration records its kind, platform condition, version and origin.

## Using an existing metadata graph

`Package::from_metadata(metadata, &id)` builds a `Package` from existing Cargo metadata. Metadata does not record source replacement settings, though.

Preparing registry packages needs the same configuration Cargo used:

```rust
use r#override::{Config, Package};

let config = Config::load_with_cwd(project)?;
let package = Package::from_metadata(metadata, &id)?.config(config);
let sources = package.prepare(destination)?;
```

`Config` is re-exported from `cargo-config2`. Packages reached through `dependency` or `resolve` share it.

If the metadata came from a command with `--config` overrides, the configuration needs the same values. For example, when the command set `source.local.local-registry` to a directory:

```rust
let mut config = Config::load_with_cwd(project)?;
config
    .source
    .entry("local".into())
    .or_default()
    .local_registry = Some(registry_directory);
```

Without a configuration, the package still supports queries; only `prepare` needs it.

## Publishing

For publishing, the output directory gets a workspace manifest listing the generated crates. Crates that depend on each other use both `path` and `version`, so Cargo publishes them in dependency order:

```sh
cd patches
cargo run
cd target/out
cargo publish --workspace
```

During development, the project can build against a generated crate before it is published:

```sh
cargo build --config 'patch.crates-io.project-assists.path="patches/target/out/assists"'
```
