# Replacing a dependency

If an application and its dependencies use Tokio, they can all use a compatible runtime through the same `tokio` imports. To try a replacement from a local checkout for one source installation:

```sh
override with --path ../my-runtime -- install my-app
```

To use a published replacement with ordinary Cargo commands, enable it for your Cargo user account:

```sh
override enable my-runtime@0.2.0
cargo build
```

Omit `@0.2.0` to let Cargo select the version. Both `with` and `enable` can also select a Git revision with `my-runtime --git <url> --rev <commit>`. `override list` shows the active replacements; `override disable my-runtime` removes this one.

## Providing a replacement

In the replacement library's `Cargo.toml`, name the upstream package and version it implements:

```toml
[package.metadata.override]
replaces = [{ package = "tokio", version = "1.54.1" }]
```

Here a dependency on `tokio` can call `my-runtime` through `tokio::spawn`. Enabling `tokio/rt` requests `my-runtime/rt`. If several packages expose the same type from `my-runtime`, their callers can exchange values of that type.

Set `git` on an entry when the original dependency comes from a Git URL. An entry can also specify `lib` if the original package has a custom library name, or `module` if the replacement exposes that API in a particular public module.

If Cargo keeps using a different version already in the project's lockfile, `cargo update -p tokio --precise 1.54.1` selects the declared version when the dependency requirements allow it. See Cargo's [patch rules](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html#the-patch-section).
