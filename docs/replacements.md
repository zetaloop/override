# Replacing a dependency

If an application and its dependencies use Tokio, they can all use a compatible runtime through the same `tokio` imports. To try a replacement from a local checkout for one source installation:

```sh
override with --path ../my-runtime -- install my-app
```

To use a published replacement across projects, enable it once in your Cargo configuration:

```sh
override enable my-runtime@0.2.0
cargo build
```

Omit `@0.2.0` to let Cargo select the version. Both `with` and `enable` can also select a Git revision with `my-runtime --git <url> --rev <commit>`. `override list` shows the active replacements; `override disable my-runtime` removes this one.

If Cargo keeps using a different version of Tokio already in the project's lockfile, `cargo update -p tokio --precise 1.54.1` selects the upstream version in the example below if the project's requirements allow it. See Cargo's [patch rules](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html#the-patch-section).

## Providing a replacement

In the replacement library's `Cargo.toml`, name the upstream package and version it implements:

```toml
[package.metadata.override]
replaces = [{ package = "tokio", version = "1.54.1" }]
```

Here `1.54.1` is the version that existing dependencies request as `tokio`. They can call `tokio::spawn` and enable `tokio/rt` to use the corresponding function and feature from `my-runtime`.

To provide several versions of the same upstream package, add an entry to `replaces` for each version.

Set `git` on an entry when the original dependency comes from a Git URL. An entry can also specify `lib` if the original package has a custom library name, or `module` if the replacement exposes that API in a particular public module.
