# override

Symbol-based Rust source editing for build scripts. Bridge crates prepare dependency sources, apply patches, and compile the result with their own Cargo configuration.

The crate is imported as `r#override`. Use `default-features = false` for the editor without Cargo integration.

## Editing

```rust
use r#override::{Edition, Result, Source, item};

fn main() -> Result<()> {
    let mut source = Source::parse("struct State { value: u32 }", Edition::Edition2024)?;
    source.select(item("State").field("value"))?.set_type("u64")?;
    println!("{source}");
    Ok(())
}
```

[examples/edit.rs](examples/edit.rs) demonstrates macro editing, declaration lookup, extraction, control-flow propagation and delegation. It prints Rust source that can be compiled and run.

Regions depend on source order: new upstream code between their anchors becomes part of the selection. Prefer whole functions, loops or branches when possible. Extraction creates a new scope; the supplied signature and arguments must account for ownership and local destruction.

## Bridge crates

A source dependency can be declared under an inactive target:

```toml
[target.'cfg(any())'.dependencies]
upstream = { package = "ra_ap_ide_assists", version = "=0.0.352" }
```

With `override` as a build dependency, `build.rs` can prepare the source:

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

The bridge's `src/lib.rs`:

```rust
include!(env!("UPSTREAM_SOURCE"));
```

Crate-level attributes belong in the bridge root because `include!` rejects inner attributes. Cargo metadata resolves the bridge's workspace, which may differ from its consumer's dependency graph; the bridge should fix its source dependency's version and origin.

Reusing an upstream `build.rs` requires a helper build dependency to prepare it before the bridge script is compiled. Its exported include macro uses the bridge's features and package environment; relative resource paths resolve against the prepared source. [tests/build.rs](tests/build.rs) contains a complete bridge workspace with this arrangement and dependency replacement.

## Development

```sh
cargo fmt
cargo fix --allow-dirty
cargo clippy --fix --allow-dirty --all-targets -- -D warnings
cargo clippy --no-default-features --all-targets -- -D warnings
cargo test
```
