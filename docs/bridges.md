# Bridge crates

A bridge is a Cargo package that prepares another package's source, applies edits and compiles the result. Its manifest describes the dependencies, features, targets and lints used for that compilation.

The `build` feature supplies `Package`, `Sources`, `Entry` and the manifest comparison helpers. It is enabled by default.

## Declare the source dependency

The bridge uses override as a build dependency:

```sh
cargo add --build override
```

An inactive target declaration lets Cargo resolve the source package while the bridge compiles the edited copy:

```toml
[target.'cfg(any())'.dependencies]
upstream = { package = "ra_ap_ide_assists", version = "=0.0.352" }
```

The alias `upstream` is the name used in `Package::dependency`. The declaration fixes the package version and origin used by the bridge. Path and Git dependencies can be declared here as well.

The bridge supplies the normal dependencies and feature definitions required by the copied source. [Comparing package configuration](#comparing-package-configuration) provides checks for this relationship.

## Prepare, edit and include

The bridge's `build.rs` selects the source package and prepares an output directory:

```rust
use std::{env, path::PathBuf};
use r#override::{Package, Result};

fn main() -> Result<()> {
    let upstream = Package::current()?.dependency("upstream")?;
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut sources = upstream.prepare(out.join("upstream"))?;

    sources.include(upstream.library()?)?.emit("UPSTREAM_SOURCE")?;
    Ok(())
}
```

Edits run between preparation and inclusion. The full editing model is described in [Editing Rust](editing.md).

The bridge's `src/lib.rs` compiles the prepared entry:

```rust
include!(env!("UPSTREAM_SOURCE"));
```

Compilation belongs to the bridge's Cargo invocation. The prepared directory contains the source and resources; the surrounding Cargo target directory contains compilation outputs.

### Source contents

Source preparation follows the package's resolved source:

| Source | Prepared input |
| :--- | :--- |
| Registry or registry mirror | The corresponding published `.crate` archive |
| Local registry | The `.crate` archive in the configured local registry |
| Vendor directory | Files listed in `.cargo-checksum.json` |
| Git or path dependency | Actual file paths from Cargo's package file listing |

Registry preparation uses published contents. Git and path preparation uses Cargo's include/exclude selection and its mappings for original manifests and package-external resources. Vendor entries refer to actual files; a missing listed file is an error.

Git and path file queries use Cargo's structured listing. The query process enables `-Z unstable-options` with `RUSTC_BOOTSTRAP=1`, so it can run with stable Cargo.

The output directory is either empty or belongs to an earlier `prepare` call. Each invocation recreates the prepared directory from its input before edits are applied. Source paths and relevant manifests are emitted as Cargo rebuild dependencies.

Separate directories produce independently editable copies:

```rust
let host = upstream.prepare(out.join("host"))?;
let guest = upstream.prepare(out.join("guest"))?;
```

### Working with files

`Sources::directory()` returns the prepared directory, and `package()` returns its package information. File operations use paths relative to that directory:

```rust
sources.edit("src/lib.rs", |source| {
    source.select(r#override::item("SCALE"))?.set_value("3")
})?;
```

This is the constant edit used by the [bridge workspace example](../tests/build.rs). Queries use the declarations present in the selected upstream package.

For declarations spread across files:

```rust
let declarations = sources.read("src/shared.rs")?;
sources.edit("src/worker.rs", |source| {
    source.set_module("worker")?;
    source.add_source("shared", &declarations);
    source.select(item("Worker::run"))?.set_visibility("pub(crate)")
})?;
```

`read` parses a source file for queries and declaration context. `edit` writes its result after the editing closure succeeds. `path` supplies a path for resource operations:

```rust
let config = sources.path("config.txt")?;
```

Parsing initially uses the package edition. `sources.target(target)?` selects a particular target's edition.

### Preparing a compilation entry

`Sources::include(target)` prepares the target entry and returns an `Entry`:

```rust
let entry = sources.include(upstream.library()?)?;
entry.emit("UPSTREAM_SOURCE")?;
```

`Entry::path` is the prepared entry path. `Entry::attributes` contains crate-level inner attributes removed from that file because `include!` rejects them at the include site.

Attributes such as `#![no_std]` and crate-level feature configuration belong in the bridge's root:

```rust
#![no_std]
include!(env!("UPSTREAM_SOURCE"));
```

`emit` supplies the entry path as a Cargo compilation environment variable. `export`, used in the [build-script arrangement](#sharing-an-upstream-build-script), writes an include macro for another crate to invoke.

## Selecting packages and targets

`Package::current()` uses a build script's `CARGO_MANIFEST_PATH`. `Package::load(manifest)` selects a manifest explicitly. Both query Cargo metadata from the manifest's directory with all features enabled.

That graph belongs to the selected workspace. A bridge distributed as a dependency can therefore have a different graph from its consumer; the bridge's own manifest defines its source dependency.

`Package::data()` exposes the Cargo package information. `directory()` and `edition()` provide its source directory and Rust edition.

```rust
let upstream = bridge.dependency("upstream")?;
let library = upstream.library()?;
let script = upstream.build_script()?;
let binary = upstream.target(r#override::TargetKind::Bin, "tool")?;
```

`library()` requires a unique library target. `target(kind, name)` identifies other targets explicitly.

When a dependency alias has several platform-dependent declarations that resolve to different packages, choose a declaration from `bridge.data().dependencies` and pass it to `bridge.resolve(declaration)?`. The declaration belongs to that package and carries its dependency kind, target condition, version and origin.

## Using an existing metadata graph

`Package::from_metadata(metadata, &id)` imports a graph for package and dependency queries. Cargo metadata omits the source-replacement configuration used to obtain it.

For registry archive preparation, supply the corresponding configuration:

```rust
use r#override::{Config, Package};

let config = Config::load_with_cwd(project)?;
let package = Package::from_metadata(metadata, &id)?.config(config);
let sources = package.prepare(destination)?;
```

`Config` is re-exported from `cargo-config2`. `Package` uses its source-replacement settings and shares them with packages selected through `dependency` or `resolve`.

When the original query used command-line configuration overrides, express those values in the supplied configuration as well. For example, a query that sets `source.local.local-registry` to an absolute directory uses:

```rust
let mut config = Config::load_with_cwd(project)?;
config
    .source
    .entry("local".into())
    .or_default()
    .local_registry = Some(registry_directory);
```

This configuration corresponds to the graph's source selection. The metadata import itself remains usable for queries before source configuration is supplied.

## Comparing package configuration

The comparison helpers check the bridge against its selected upstream package:

```rust
use r#override::{DependencyKind, Package, check_target};

let bridge = Package::current()?;
let upstream = bridge.dependency("upstream")?;
let replacements = [
    ("dependency", bridge.dependency("replacement")?),
];

bridge.check_dependencies(&upstream, DependencyKind::Normal, &replacements)?;
bridge.check_features(&upstream, &replacements)?;
bridge.check_lints(&upstream)?;
check_target(upstream.library()?, bridge.library()?)?;
```

Each replacement key is an upstream dependency alias. Its value is the actual package selected through a bridge dependency; that selection also carries the bridge alias.

| Helper | Compared information |
| :--- | :--- |
| `check_dependencies` | Dependency kind, target condition, package identity or declaration, optionality, default features and enabled features |
| `check_features` | Upstream feature definitions after dependency-alias replacement; bridge-owned additions are accepted |
| `check_lints` | Upstream lint levels and priorities, including workspace inheritance |
| `check_target` | Edition, crate types and required features |

`DependencyKind::Build` and `DependencyKind::Development` select those dependency groups. Additional bridge dependencies and targets belong to the bridge's configuration.

These checks compare configuration. Including upstream source still gives it the bridge's compilation environment, including `cfg!(feature)`, `env!("CARGO_PKG_NAME")` and `CARGO_MANIFEST_DIR`.

## Sharing an upstream build script

A bridge's build script must be prepared before Cargo compiles that script. A helper build dependency provides this ordering:

```text
support/build.rs  prepares upstream/build.rs and exports an include macro
support/lib.rs    exposes the generated macro
bridge/build.rs   invokes the macro and runs the upstream entry
```

The support package declares its own inactive source dependency and uses override in its build dependencies. Its `build.rs` prepares the upstream script:

```rust
use std::{env, path::PathBuf};
use r#override::{Package, Result, item};

fn main() -> Result<()> {
    let upstream = Package::current()?.dependency("upstream")?;
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut sources = upstream.prepare(out.join("source"))?;
    sources.edit("build.rs", |source| {
        source.select(item("main"))?.set_visibility("pub")
    })?;
    sources
        .include(upstream.build_script()?)?
        .export("include_build", out.join("script.rs"))?;
    Ok(())
}
```

The support package's `src/lib.rs` exposes the macro:

```rust
include!(concat!(env!("OUT_DIR"), "/script.rs"));
```

After declaring the support package as a build dependency, the bridge invokes the macro in its own `build.rs`:

```rust
mod upstream {
    support::include_build!();
}

fn main() {
    upstream::main();
}
```

The included script uses the bridge's features and package environment. Relative resources such as `include_str!("config.txt")` are resolved from the prepared source file; the script's working directory and `CARGO_MANIFEST_DIR` refer to the bridge.

[tests/build.rs](../tests/build.rs) constructs and runs this arrangement, including dependency replacement, a source edit and a resource change between builds.
