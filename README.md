# override

override lets you patch a Rust dependency by naming the code you want to change. This patch changes the type of the `value` field in `State`:

```rust
use r#override::{Edition, Result, Source, item};

fn main() -> Result<()> {
    let mut source = Source::parse("struct State { value: u32 }", Edition::Edition2024)?;
    source.select(item("State").field("value"))?.set_type("u64")?;
    println!("{source}");
    Ok(())
}
```

The CLI can also build a project against a compatible replacement library. To try one from a local checkout:

```sh
cargo install override
override with --path ../my-runtime -- build
```

## Documentation

- [Editing Rust](docs/editing.md): selecting targets, supplying declarations, changing source and reusing upstream logic.
- [Patch workspaces](docs/patches.md): preparing dependency sources and publishing the edited packages.
- [Replacing a dependency](docs/replacements.md): using a replacement library or providing one for other projects.
- [Editing example](examples/edit.rs): extending a command dispatcher by reusing its output implementation.
