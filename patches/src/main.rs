use std::{env, fs, path::Path};

use r#override::{Manifest, Package, Result, Sources, root};
use toml::{Table, Value};

mod data;

fn main() -> Result<()> {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/out");
    let patches = Package::load(env!("CARGO_MANIFEST_PATH"))?;
    let version = include_str!("../../Cargo.toml")
        .parse::<Table>()?
        .get("package")
        .and_then(Value::as_table)
        .and_then(|package| package.get("version"))
        .and_then(Value::as_str)
        .ok_or("root manifest has no package version")?
        .to_owned();
    let syntax = patches.dependency("ra_ap_syntax")?;
    let parser = syntax.dependency("parser")?;
    let lexer = syntax.dependency("ra-ap-rustc_lexer")?;

    let mut sources = prepare(
        &lexer,
        &out.join("override-lexer"),
        "override-lexer",
        &version,
    )?;
    sources.edit("src/lib.rs", |source| {
        source.select(root())?.add_use(
            "use crate::unicode::{ident as unicode_ident, properties as unicode_properties};",
        )?;
        source
            .select(root())?
            .mount_module("mod unicode", "unicode.rs")
    })?;
    fs::write(sources.path("src/unicode.rs")?, include_str!("unicode.rs"))?;
    fs::write(sources.path("src/unicode_data.rs")?, data::generate()?)?;
    fs::copy(
        patches
            .dependency("unicode-ident")?
            .directory()
            .join("LICENSE-UNICODE"),
        sources.path("LICENSE-UNICODE")?,
    )?;
    sources.manifest(|manifest| {
        manifest["package"]["license"] = "MIT AND Unicode-3.0".into();
        let dependencies = table(manifest, "dependencies")?;
        dependencies.remove("unicode-ident");
        dependencies.remove("unicode-properties");
        Ok(())
    })?;

    for (upstream, name, replaced, replacement) in [
        (
            &parser,
            "override-parser",
            "ra-ap-rustc_lexer",
            "override-lexer",
        ),
        (
            &syntax,
            "override-syntax",
            "ra-ap-rustc_lexer",
            "override-lexer",
        ),
    ] {
        let mut sources = prepare(upstream, &out.join(name), name, &version)?;
        sources.manifest(|manifest| {
            let dependencies = table(manifest, "dependencies")?;
            redirect(dependencies, replaced, replacement, &version)?;
            if name == "override-syntax" {
                redirect(dependencies, "parser", "override-parser", &version)?;
                dependencies.remove("unicode-ident");
                manifest["package"]
                    .as_table_mut()
                    .ok_or("package is not a table")?
                    .remove("metadata");
            }
            Ok(())
        })?;
    }

    fs::write(
        out.join("Cargo.toml"),
        "[workspace]\nmembers = [\"override-lexer\", \"override-parser\", \"override-syntax\"]\nresolver = \"3\"\n",
    )?;
    Ok(())
}

fn prepare(upstream: &Package, out: &Path, name: &str, version: &str) -> Result<Sources> {
    let mut sources = upstream.prepare(out)?;
    for file in ["Cargo.toml.orig", ".cargo_vcs_info.json", "Cargo.lock"] {
        let path = sources.path(file)?;
        if path.try_exists()? {
            fs::remove_file(path)?;
        }
    }
    fs::write(
        sources.path("LICENSE-MIT")?,
        format!(
            "Copyright (c) The Rust Project Contributors\nCopyright (c) 2026 zetaloop\n\n{}",
            include_str!("../../LICENSE")
                .split_once("Permission")
                .map(|(_, notice)| format!("Permission{notice}"))
                .ok_or("LICENSE has no permission notice")?
        ),
    )?;
    let data = upstream.data();
    sources.manifest(|manifest| {
        let package = table(manifest, "package")?;
        package.insert("name".into(), name.into());
        package.insert("version".into(), version.into());
        package.insert("license".into(), "MIT".into());
        package.insert(
            "description".into(),
            format!("{} {} for override", data.name, data.version).into(),
        );
        package.insert(
            "repository".into(),
            "https://github.com/zetaloop/override".into(),
        );
        package.insert(
            "exclude".into(),
            Value::Array(vec![".override-source".into()]),
        );
        Ok(())
    })?;
    Ok(sources)
}

fn table<'a>(manifest: &'a mut Manifest, name: &str) -> Result<&'a mut Manifest> {
    manifest
        .get_mut(name)
        .and_then(Value::as_table_mut)
        .ok_or_else(|| format!("manifest has no {name} table").into())
}

fn redirect(dependencies: &mut Manifest, alias: &str, package: &str, version: &str) -> Result<()> {
    let dependency = dependencies
        .get_mut(alias)
        .and_then(Value::as_table_mut)
        .ok_or_else(|| format!("manifest has no dependency `{alias}`"))?;
    dependency.insert("package".into(), package.into());
    dependency.insert("version".into(), format!("={version}").into());
    dependency.insert("path".into(), format!("../{package}").into());
    Ok(())
}
