use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use cargo_metadata::{MetadataCommand, semver::Version};
use r#override::{Manifest, Package, Result};
use ra_ap_syntax::{
    Edition::Edition2024,
    utils::{is_identifier, is_raw_identifier},
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Replacement {
    package: String,
    version: Version,
    git: Option<String>,
    lib: Option<String>,
    module: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct Fragment {
    pub patch: BTreeMap<String, BTreeMap<String, Entry>>,
}

#[derive(Deserialize, Serialize)]
pub struct Entry {
    path: PathBuf,
    pub version: Version,
}

impl Fragment {
    pub fn resolve(&self, config: &Path) -> Result<()> {
        let entry = self
            .patch
            .values()
            .flat_map(BTreeMap::values)
            .next()
            .unwrap();
        MetadataCommand::new()
            .current_dir(&entry.path)
            .manifest_path(entry.path.join("Cargo.toml"))
            .other_options(vec![
                "--config".into(),
                config.to_str().ok_or("invalid Cargo config path")?.into(),
            ])
            .exec()?;
        Ok(())
    }
}

pub struct Provider {
    package: cargo_metadata::Package,
    dependency: Manifest,
    replaces: Vec<Replacement>,
}

impl Provider {
    pub fn load(args: &[OsString]) -> Result<Self> {
        let mut dependency = Manifest::new();
        let package = match args {
            [flag, dir] if flag == "--path" => {
                let manifest = fs::canonicalize(Path::new(dir).join("Cargo.toml"))?;
                let metadata = MetadataCommand::new()
                    .manifest_path(&manifest)
                    .current_dir(manifest.parent().unwrap())
                    .no_deps()
                    .exec()?;
                let package = metadata
                    .packages
                    .into_iter()
                    .find(|p| fs::canonicalize(&p.manifest_path).is_ok_and(|path| path == manifest))
                    .ok_or("--path must name a package directory")?;
                dependency.insert(
                    "path".into(),
                    package.manifest_path.parent().unwrap().as_str().into(),
                );
                package
            }
            _ => {
                let (name, version) = match args {
                    [spec] => match super::text(spec)?.split_once('@') {
                        Some((name, version)) => {
                            (name, format!("={}", version.parse::<Version>()?))
                        }
                        None => (super::text(spec)?, "*".into()),
                    },
                    [name, git, url, rev, commit] if git == "--git" && rev == "--rev" => {
                        dependency.insert("git".into(), super::text(url)?.into());
                        dependency.insert("rev".into(), super::text(commit)?.into());
                        (super::text(name)?, "*".into())
                    }
                    _ => return Err(super::USAGE.into()),
                };
                dependency.insert("package".into(), name.into());
                dependency.insert("version".into(), version.into());
                let directory = tempfile::tempdir()?;
                let manifest = directory.path().join("Cargo.toml");
                let query = toml::toml! {
                    [workspace]
                    [package]
                    name = "override-query"
                    version = "0.0.0"
                    edition = "2024"
                    [lib]
                    path = "lib.rs"
                    [dependencies]
                    implementation = (dependency.clone())
                };
                fs::write(directory.path().join("lib.rs"), "")?;
                fs::write(&manifest, toml::to_string(&query)?)?;
                Package::load(manifest)?
                    .dependency("implementation")?
                    .data()
                    .clone()
            }
        };
        if !package.targets.iter().any(|target| {
            target.is_lib() || target.is_rlib() || target.is_dylib() || target.is_proc_macro()
        }) {
            return Err(format!("{} has no Rust library to re-export", package.name).into());
        }
        let declarations = package.metadata["override"]
            .get("replaces")
            .ok_or_else(|| {
                format!(
                    "{} needs a nonempty package.metadata.override.replaces array",
                    package.name
                )
            })?;
        let replaces = Vec::<Replacement>::deserialize(declarations)?;
        if replaces.is_empty() {
            return Err(format!("{} declares no replacements", package.name).into());
        }
        dependency.insert("package".into(), package.name.to_string().into());
        dependency.insert("version".into(), format!("={}", package.version).into());
        dependency.insert("default-features".into(), false.into());
        Ok(Self {
            package,
            dependency,
            replaces,
        })
    }

    pub fn write(&self, directory: &Path) -> Result<PathBuf> {
        let mut config = Fragment::default();
        let features: Manifest = self
            .package
            .features
            .keys()
            .map(|name| {
                (
                    name.to_string(),
                    vec![format!("implementation/{name}")].into(),
                )
            })
            .collect();
        for (i, target) in self.replaces.iter().enumerate() {
            let source = target.git.as_deref().unwrap_or("crates-io");
            let path = directory.join(i.to_string());
            let mut manifest = toml::toml! {
                [workspace]
                [package]
                name = (target.package.clone())
                version = (target.version.to_string())
                edition = "2024"
                publish = false
                [lib]
                path = "lib.rs"
                [dependencies]
                implementation = (self.dependency.clone())
            };
            if let Some(lib) = &target.lib {
                manifest["lib"]
                    .as_table_mut()
                    .unwrap()
                    .insert("name".into(), lib.clone().into());
            }
            manifest.insert("features".into(), features.clone().into());
            let export = match &target.module {
                Some(module) => {
                    if !module.split("::").all(|part| {
                        let ident = part.strip_prefix("r#").unwrap_or(part);
                        ident != "_"
                            && (is_identifier(ident, Edition2024)
                                || (ident != part && is_raw_identifier(ident, Edition2024)))
                    }) {
                        return Err(format!("invalid Rust module path `{module}`").into());
                    }
                    format!("pub use ::implementation::{module}::*;\n")
                }
                None => "pub use ::implementation::*;\n".into(),
            };
            fs::create_dir(&path)?;
            fs::write(path.join("Cargo.toml"), toml::to_string(&manifest)?)?;
            fs::write(path.join("lib.rs"), format!("#![no_std]\n\n{export}"))?;
            let entries = config.patch.entry(source.into()).or_default();
            if entries
                .insert(
                    target.package.clone(),
                    Entry {
                        path,
                        version: target.version.clone(),
                    },
                )
                .is_some()
            {
                return Err(
                    format!("duplicate replacement for {} from {source}", target.package).into(),
                );
            }
        }
        let path = directory.join(format!(
            "{}@{}.toml",
            self.package.name, self.package.version
        ));
        fs::write(&path, toml::to_string(&config)?)?;
        Ok(path)
    }
}
