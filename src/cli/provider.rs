use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    hash::{DefaultHasher, Hash, Hasher},
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

#[derive(Default, Deserialize, Hash, Serialize)]
pub struct Fragment {
    patch: BTreeMap<String, BTreeMap<String, Entry>>,
}

#[derive(Deserialize, Hash, Serialize)]
struct Entry {
    path: PathBuf,
    package: Option<String>,
    version: Version,
}

impl Fragment {
    pub fn targets(&self) -> impl Iterator<Item = (&str, &str, &Version)> {
        self.patch.iter().flat_map(|(source, entries)| {
            entries.iter().map(move |(key, entry)| {
                (
                    source.as_str(),
                    entry.package.as_deref().unwrap_or(key),
                    &entry.version,
                )
            })
        })
    }

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

    pub fn prepare(&self, home: &Path) -> Result<PathBuf> {
        let mut config = Fragment::default();
        let mut crates = Vec::new();
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
            crates.push((
                toml::to_string(&manifest)?,
                format!("#![no_std]\n\n{export}"),
            ));
            let entries = config.patch.entry(source.into()).or_default();
            let version = target
                .version
                .to_string()
                .replace('.', "_")
                .replace('+', "__");
            if entries
                .insert(
                    format!("{}___{version}", target.package),
                    Entry {
                        path: i.to_string().into(),
                        package: Some(target.package.clone()),
                        version: target.version.clone(),
                    },
                )
                .is_some()
            {
                return Err(format!(
                    "duplicate replacement for {}@{} from {source}",
                    target.package, target.version
                )
                .into());
            }
        }
        let mut key = DefaultHasher::new();
        (
            &self.package.id,
            self.package.source.as_ref().map(|s| &s.repr),
            &crates,
            &config,
        )
            .hash(&mut key);
        let cache = home.join("override");
        let directory = cache.join(format!("entry-{:016x}", key.finish()));
        let name = format!("{}@{}.toml", self.package.name, self.package.version);
        let path = directory.join(&name);
        if !path.try_exists()? {
            fs::create_dir_all(&cache)?;
            let temp = tempfile::tempdir_in(cache)?;
            for (i, (manifest, export)) in crates.iter().enumerate() {
                let entry = temp.path().join(i.to_string());
                fs::create_dir(&entry)?;
                fs::write(entry.join("Cargo.toml"), manifest)?;
                fs::write(entry.join("lib.rs"), export)?;
            }
            for entry in config.patch.values_mut().flat_map(BTreeMap::values_mut) {
                entry.path = directory.join(&entry.path);
            }
            fs::write(temp.path().join(name), toml::to_string(&config)?)?;
            if let Err(error) = fs::rename(temp.path(), &directory)
                && !path.try_exists()?
            {
                return Err(error.into());
            }
        }
        Ok(path)
    }
}
