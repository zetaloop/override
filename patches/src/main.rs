use std::{env, fs, path::Path};

use r#override::{Manifest, Package, Result, Sources, root};
use toml::Value;

mod data;
mod stdlib;

fn main() -> Result<()> {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/out");
    let patches = Package::load(env!("CARGO_MANIFEST_PATH"))?;
    let version = env!("CARGO_PKG_VERSION");
    let syntax = patches.dependency("ra_ap_syntax")?;
    let mbe = patches.dependency("ra_ap_mbe")?;
    let packages = [
        ("override-lexer", syntax.dependency("ra-ap-rustc_lexer")?),
        ("override-parser", syntax.dependency("parser")?),
        ("override-syntax", syntax),
        ("override-span", mbe.dependency("span")?),
        ("override-tt", mbe.dependency("tt")?),
        ("override-syntax-bridge", mbe.dependency("syntax-bridge")?),
        ("override-mbe", mbe),
    ];

    for &(name, ref upstream) in &packages {
        let mut sources = prepare(upstream, &out.join(name), name, version)?;
        if name == "override-lexer" {
            embed_unicode(&mut sources, &patches)?;
        }
        if name == "override-mbe" {
            stdlib::embed(&mut sources)?;
        }
        sources.manifest(|manifest| {
            let dependencies = table(manifest, "dependencies")?;
            if matches!(name, "override-lexer" | "override-syntax") {
                dependencies.remove("unicode-ident");
                dependencies.remove("unicode-properties");
            }
            for (alias, entry) in dependencies {
                let package = entry
                    .get("package")
                    .and_then(Value::as_str)
                    .unwrap_or(alias);
                if let Some(&(replacement, _)) = packages
                    .iter()
                    .find(|(_, p)| p.data().name.as_str() == package)
                {
                    let dep = entry
                        .as_table_mut()
                        .ok_or_else(|| format!("invalid dependency `{alias}`"))?;
                    dep.insert("package".into(), replacement.into());
                    dep.insert("version".into(), format!("={version}").into());
                    dep.insert("path".into(), format!("../{replacement}").into());
                }
            }
            if name == "override-syntax" {
                table(manifest, "package")?.remove("metadata");
            }
            Ok(())
        })?;
    }

    let members: Vec<_> = packages.iter().map(|(name, _)| *name).collect();
    let workspace = toml::toml! {
        [workspace]
        members = (members)
        resolver = "3"
    };
    fs::write(out.join("Cargo.toml"), toml::to_string(&workspace)?)?;
    Ok(())
}

fn embed_unicode(sources: &mut Sources, patches: &Package) -> Result<()> {
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
        let license = if name == "override-lexer" {
            "MIT AND Unicode-3.0"
        } else {
            "MIT"
        };
        package.insert("license".into(), license.into());
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
