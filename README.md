# override

Symbol-based Rust source editing and dependency patching. Patch workspaces prepare dependency sources, apply patches, and write the result as crates for publishing.

Queries identify Rust declarations, calls and their relationships. Edits use Rust fragments such as field declarations, types and function signatures. This makes patches follow the structure they depend on as upstream source changes.

The crate is imported as `r#override`. The editor can be used independently with `default-features = false`; the default `build` feature provides Cargo integration.

```rust
use r#override::{Edition, Result, Source, item};

fn main() -> Result<()> {
    let mut source = Source::parse("struct State { value: u32 }", Edition::Edition2024)?;
    source.select(item("State").field("value"))?.set_type("u64")?;
    println!("{source}");
    Ok(())
}
```

## Documentation

- [Editing Rust](docs/editing.md): selecting targets, supplying declarations, changing source and reusing upstream logic.
- [Patch workspaces](docs/patches.md): preparing dependency sources and publishing the edited packages.
- [Editing example](examples/edit.rs): extending a command dispatcher by reusing its output implementation.

## Development

```sh
cargo fmt
cargo fix --allow-dirty
cargo clippy --fix --allow-dirty --all-targets -- -D warnings
cargo clippy --no-default-features --all-targets -- -D warnings
```
