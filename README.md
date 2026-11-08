# override

Symbol-based Rust source editing and dependency patching. A patch workspace edits upstream packages and writes them as crates for publishing.

Queries locate code by its declarations, calls and their relationships, and edits use Rust fragments such as fields, types and function signatures. Because patches follow structure instead of line positions, they keep working as upstream source changes.

The crate is imported as `r#override`. Without default features it provides only the editor; the default `build` feature adds Cargo integration.

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
