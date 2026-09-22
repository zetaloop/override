# override

Structural Rust source editing and Cargo build tools. A bridge crate prepares dependency sources, applies symbol-based edits, and compiles the result with its own Cargo configuration.

Rust code imports the crate as `r#override`. The default `build` feature provides Cargo integration; `default-features = false` selects the standalone editor.

## Editing Rust

```rust
use r#override::{Edition, Result, Source, item};

fn main() -> Result<()> {
    let mut source = Source::parse(
        "struct State { value: u32 }",
        Edition::Edition2024,
    )?;
    source.select(item("State"))?
        .add_field("pub(crate) shared: SharedRuntime")?;
    source.select(item("State").field("value"))?
        .set_type("u64")?;
    println!("{source}");
    Ok(())
}
```

`Source` uses `ra_ap_syntax`'s full-fidelity syntax tree. Selectors identify declarations and structural relationships. Editing methods parse their inputs as the corresponding Rust syntax: a field declaration, type, visibility, expression, or function signature.

A selection identifies one object. Missing and ambiguous targets produce errors. Editing consumes that selection; the next query sees the modified source.

### Composing selectors

```rust
item("GlobalState::snapshot")
    .record("GlobalStateSnapshot")
    .field("analysis")
```

```rust
item("Runtime::block_on_inner")
    .arm("Scheduler::CurrentThread")
    .call("block_on")
```

```rust
item("GlobalState::compute_priming_scope")
    .for_loop()
    .has(r#override::arm("ProjectWorkspaceKind::Cargo"))
```

| Objects and relationships | Selectors |
| :--- | :--- |
| Declarations and members | `item`, `implementation`, `field`, `variant`, `parameter`, `generic`, `import`, `attribute` |
| Calls and control flow | `call`, `argument`, `closure`, `arm`, `for_loop`, `while_loop`, `loop_expr`, `if_expr` |
| Structural regions | `body`, `condition`, `tail_after` |
| Context and composition | `has`, `child`, `and`, `or`, `not`, `implementing`, `of_type`, `references`, `macro_call` |

Queries also inspect Rust items and blocks inside macro token trees. Edits are written back through the enclosing token trees.

### Editing selected objects

```rust
source.select(item("GlobalState::snapshot")
    .record("GlobalStateSnapshot")
    .field("analysis"))?
    .set_value("self.shared.analysis()")?;
```

Declaration operations include `rename`, `set_visibility`, `add_attribute`, `add_field`, `add_variant`, `add_parameter`, `add_generic`, `set_type`, `set_return_type`, `set_signature`, `set_bounds`, `set_where_clause`, and `remove`. Calls provide `redirect`, `add_argument`, and named argument selection; modules provide `add_use` and `mount_module`.

`rename` edits declaration names or import aliases. `redirect` changes a selected call, import, or macro path. Rust's compiler checks the resulting types, borrows, and trait requirements.

### Extracting and delegating

```rust
source.select(item("GlobalState::update_diagnostics")
    .tail_after(["generation", "subscriptions"]))?
    .extract(
        "fn spawn_native_diagnostics(&mut self, generation: DiagnosticsGeneration, subscriptions: std::sync::Arc<[FileId]>)",
        &["generation", "subscriptions"],
    )?;
```

`tail_after` selects the function suffix following the named bindings visible at its end. Further selectors operate within that region. `extract` moves a selected body, branch, loop, or symbolic region into the supplied function signature and creates the call using the supplied arguments.

```rust
source.select(item("Runtime::block_on_inner")
    .arm("Scheduler::CurrentThread")
    .call("block_on"))?
    .delegate("telekio::block_on", &["&self.blocking_pool"])?;
```

Call delegation passes the supplied context first, followed by the original method receiver and arguments. Explicit call generics are carried to the helper. Closure delegation passes the original closure after the context. Function and tail-region delegation wrap the selected code in a closure or async block.

Extraction checks control-flow destinations. A region containing an exit to its enclosing function or loop needs a selection that carries that destination, such as the function tail or the complete loop.

### Supplying declarations

Argument selection follows available function declarations, module paths, import aliases, and local bindings. Method queries use declared receiver types and impl information. `Source::set_module` names the current module; `add_source` supplies declarations from another module.

An explicit declaration supplies parameter names when a call's interface comes from outside the available sources:

```rust
use r#override::Declaration;

let dispatch = Declaration::parse(
    "fn dispatch(on_error: ErrorHandler, on_ready: ReadyHandler)",
    source.edition(),
)?;
source.select(item("run")
    .call("dispatch")
    .declared_by(&dispatch)
    .argument("on_ready")
    .closure())?
    .delegate("decorate", &["context"])?;
```

`Source::describe` accepts interface declarations and binding patterns. Named destructuring and getter field accesses can associate names with tuple fields:

```rust
source.describe("Pair(left, right)")?;
source.select(item("Pair").field("right"))?.set_type("u16")?;
```

These descriptions express the patch author's interface and layout knowledge. Declaration association uses source relationships; full Rust type inference remains the compiler's work.

[examples/edit.rs](examples/edit.rs) produces a Rust program combining these operations, with executable behavior assertions in its generated `main`.

## Cargo integration

The bridge's Cargo manifest expresses its dependencies, features, and targets. It also fixes the source package's version and origin. A source dependency can use an inactive target declaration:

```toml
[target.'cfg(any())'.dependencies]
upstream = { package = "ra_ap_ide_assists", version = "=0.0.352" }
```

With `override` as a build dependency, the bridge's `build.rs` can prepare its entry:

```rust
use std::{env, path::PathBuf};
use r#override::{DependencyKind, Package, Result, check_target};

fn main() -> Result<()> {
    let bridge = Package::current()?;
    let upstream = bridge.dependency("upstream")?;
    let replacements = [("ide-db", bridge.dependency("ide-db")?)];

    bridge.check_dependencies(&upstream, DependencyKind::Normal, &replacements)?;
    bridge.check_features(&upstream, &replacements)?;
    bridge.check_lints(&upstream)?;
    check_target(upstream.library()?, bridge.library()?)?;

    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut sources = upstream.prepare(out.join("upstream"))?;
    sources.include(upstream.library()?)?.emit("UPSTREAM_SOURCE")?;
    Ok(())
}
```

Replacement keys name upstream dependency aliases. Each replacement value identifies the bridge's actual dependency, including its package, source, version, and alias. Dependency checks compare platform conditions, package identities, optionality, default features, and dependency features. Feature checks allow additions belonging to the bridge; lint checks resolve workspace inheritance. Target checks compare editions, crate types, and required features.

The bridge's `src/lib.rs` includes the entry beneath its own root attributes:

```rust
include!(env!("UPSTREAM_SOURCE"));
```

`Sources::include` returns the removed inner attributes in `Entry::attributes`. Attributes such as `#![no_std]`, crate features, and crate-level configuration belong in the bridge's root. The project composes upstream build scripts and supplies any resource or version identity its code expects.

### Source copies

`Package::prepare` supports registry, Git, and path sources. Registry files come from Cargo's unpacked package; Git and path files follow Cargo's package inclusion rules. Prepared output directories are refreshed for each build, and source inputs are emitted as Cargo rebuild dependencies.

`Sources::edit(path, closure)` provides the same `Source` editor used independently. `read` loads another source file for declaration queries, and `path` provides a path within the copy. Parsing uses the package's edition; `Sources::target` selects a target's edition when needed.

One source package can produce multiple independently edited copies:

```rust
let host = tokio.prepare(out.join("host"))?;
let guest = tokio.prepare(out.join("guest"))?;
```

`Package::load` selects a manifest and loads its owning workspace with all features for inspection. `Package::current` uses the build script's `CARGO_MANIFEST_PATH`. The resulting graph belongs to that workspace. Source inputs are controlled by the bridge, including when the bridge is distributed as a dependency.

`Package::from_metadata` accepts a graph configured by the caller. `data` exposes Cargo's package information, and `resolve` selects a specific dependency declaration when an alias has several platform-dependent resolutions.

### Sharing an upstream build script

A helper build dependency prepares the script before the bridge's `build.rs` is compiled. Its build script can export the source entry as a macro:

```rust
let support = Package::current()?;
let upstream = support.dependency("upstream")?;
let mut sources = upstream.prepare(out.join("upstream"))?;
sources.edit("build.rs", |file| {
    file.select(r#override::item("main"))?.set_visibility("pub")
})?;
sources.include(upstream.build_script()?)?
    .export("include_build", out.join("upstream_build.rs"))?;
```

The helper's `src/lib.rs` exports the generated macro:

```rust
include!(concat!(env!("OUT_DIR"), "/upstream_build.rs"));
```

The bridge compiles the script in its own context:

```rust
mod upstream {
    build_support::include_build!();
}

fn main() {
    upstream::main();
}
```

`cfg!(feature)` and `env!("CARGO_PKG_NAME")` then use the bridge's compilation environment. `include_str!` resolves relative to the prepared source. `CARGO_MANIFEST_DIR` and the script's working directory refer to the bridge.

[tests/build.rs](tests/build.rs) builds and runs a complete workspace using this arrangement, including dependency replacement and a resource update between builds.

## Development

```sh
cargo fmt
cargo fix --allow-dirty
cargo clippy --fix --allow-dirty --all-targets -- -D warnings
cargo clippy --no-default-features --all-targets -- -D warnings
cargo test
cargo run --example edit
```

The editing example writes generated Rust to stdout. Compiling and running that output executes its behavior assertions.
