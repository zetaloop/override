use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Component, Path, PathBuf},
};

use cargo_metadata::Target;
use flate2::read::GzDecoder;

use crate::{
    Edition, Manifest, Package, Result, Source,
    package::{edition, output},
};

const OWNER: &str = "Source directory prepared by override.\n";
const MARKER: &str = ".override-source";

pub struct Sources {
    package: Package,
    directory: PathBuf,
    edition: Edition,
}

impl Package {
    pub fn prepare(&self, destination: impl AsRef<Path>) -> Result<Sources> {
        let source = fs::canonicalize(self.directory())?;
        let mut files = BTreeMap::new();
        let mut archive = None;
        if let Some(registry) = self.data().source.as_ref().and_then(|source| {
            source.repr.strip_prefix("registry+").or_else(|| {
                source
                    .repr
                    .starts_with("sparse+")
                    .then_some(source.repr.as_str())
            })
        }) {
            let checksum = source.join(".cargo-checksum.json");
            if checksum.try_exists()? {
                let metadata: serde_json::Value =
                    serde_json::from_reader(fs::File::open(&checksum)?)?;
                files.extend(
                    metadata["files"]
                        .as_object()
                        .ok_or("directory source has no file list")?
                        .keys()
                        .map(|name| (PathBuf::from(name), source.join(name))),
                );
            } else {
                let config = self.config.as_ref().ok_or(
                    "registry source configuration is unavailable; supply it with config()",
                )?;
                let mut configured = config.iter().find_map(|(name, source)| {
                    (source.registry.as_deref() == Some(registry)
                        || name == "crates-io"
                            && matches!(
                                registry,
                                "https://github.com/rust-lang/crates.io-index"
                                    | "sparse+https://index.crates.io/"
                            ))
                    .then_some(source)
                });
                let mut visited = BTreeSet::new();
                while let Some(name) = configured.and_then(|source| source.replace_with.as_deref())
                {
                    if !visited.insert(name) {
                        return Err(format!("cyclic source replacement at `{name}`").into());
                    }
                    configured = config.get(name);
                }
                let directory = if let Some(directory) =
                    configured.and_then(|source| source.local_registry.as_ref())
                {
                    directory.clone()
                } else {
                    let registry = source.parent().ok_or("source has no registry directory")?;
                    registry
                        .parent()
                        .and_then(Path::parent)
                        .ok_or("source has no registry root")?
                        .join("cache")
                        .join(
                            registry
                                .file_name()
                                .ok_or("registry directory has no name")?,
                        )
                };
                let path = directory.join(format!(
                    "{}-{}.crate",
                    self.data().name,
                    self.data().version
                ));
                let input = fs::File::open(&path)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                archive = Some(tar::Archive::new(GzDecoder::new(input)));
            }
        } else {
            let listing = output(
                self.command(&[
                    "-Z",
                    "unstable-options",
                    "package",
                    "--list",
                    "--allow-dirty",
                    "--message-format=json",
                    "--package",
                    self.data().name.as_ref(),
                ])
                .env("RUSTC_BOOTSTRAP", "1"),
            )?;
            let listing: serde_json::Value = serde_json::from_str(&listing)?;
            files.extend(
                listing["files"]
                    .as_object()
                    .ok_or("package output has no file list")?
                    .iter()
                    .filter_map(|(name, file)| {
                        file["path"]
                            .as_str()
                            .map(|path| (PathBuf::from(name), PathBuf::from(path)))
                    }),
            );
        }
        let destination = destination.as_ref();
        if destination.try_exists()? && fs::symlink_metadata(destination)?.file_type().is_symlink()
        {
            return Err("source output must be a directory, not a symbolic link".into());
        }
        fs::create_dir_all(destination)?;
        let destination = fs::canonicalize(destination)?;
        if source.starts_with(&destination)
            || self.graph.packages.iter().any(|package| {
                fs::canonicalize(&package.manifest_path)
                    .is_ok_and(|manifest| manifest.starts_with(&destination))
            })
        {
            return Err("source output contains a package input".into());
        }
        if fs::read_dir(&destination)?.next().transpose()?.is_some() {
            if fs::read_to_string(destination.join(MARKER)).ok().as_deref() != Some(OWNER) {
                return Err(format!(
                    "{} is not a prepared source directory",
                    destination.display()
                )
                .into());
            }
            fs::remove_dir_all(&destination)?;
            fs::create_dir(&destination)?;
        }
        fs::write(destination.join(MARKER), OWNER)?;
        if let Some(mut archive) = archive {
            let prefix = format!("{}-{}", self.data().name, self.data().version);
            for entry in archive.entries()? {
                let mut entry = entry?;
                if entry.header().entry_type().is_dir() {
                    continue;
                }
                if !entry.header().entry_type().is_file() {
                    return Err("package archive contains a non-file entry".into());
                }
                let path = entry.path()?;
                let relative = path.strip_prefix(&prefix)?;
                relative_path(relative)?;
                let output = destination.join(relative);
                fs::create_dir_all(output.parent().ok_or("source file has no parent")?)?;
                let mut output = fs::File::create(output)?;
                io::copy(&mut entry, &mut output)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    output.set_permissions(fs::Permissions::from_mode(
                        entry.header().mode()? | 0o200,
                    ))?;
                }
            }
        }
        for (relative, original) in files {
            relative_path(&relative)?;
            if original.starts_with(&destination) {
                continue;
            }
            let output = destination.join(&relative);
            fs::create_dir_all(output.parent().ok_or("source file has no parent")?)?;
            let mut input = fs::File::open(&original)
                .map_err(|error| format!("{}: {error}", original.display()))?;
            let mut output = fs::File::create(&output)?;
            io::copy(&mut input, &mut output)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                output.set_permissions(fs::Permissions::from_mode(
                    input.metadata()?.permissions().mode() | 0o200,
                ))?;
            }
        }
        Ok(Sources {
            package: self.clone(),
            directory: destination,
            edition: self.edition()?,
        })
    }
}

impl Sources {
    pub fn package(&self) -> &Package {
        &self.package
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn target(&mut self, target: &Target) -> Result<&mut Self> {
        if !self.package.data().targets.contains(target) {
            return Err("target belongs to a different package".into());
        }
        self.edition = edition(target.edition)?;
        Ok(self)
    }

    pub fn path(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        relative_path(relative.as_ref())?;
        Ok(self.directory.join(relative))
    }

    pub fn read(&self, relative: impl AsRef<Path>) -> Result<Source> {
        Source::parse(&fs::read_to_string(self.path(relative)?)?, self.edition)
    }

    pub fn edit(
        &mut self,
        relative: impl AsRef<Path>,
        edit: impl FnOnce(&mut Source) -> Result<()>,
    ) -> Result<()> {
        let path = self.path(relative)?;
        let original = fs::read_to_string(&path)?;
        let mut source = Source::parse(&original, self.edition)?;
        edit(&mut source).map_err(|error| format!("{}: {error}", path.display()))?;
        let edited = source.to_string();
        if edited != original {
            fs::write(path, edited)?;
        }
        Ok(())
    }

    pub fn manifest(&mut self, edit: impl FnOnce(&mut Manifest) -> Result<()>) -> Result<()> {
        let path = self.directory.join("Cargo.toml");
        let mut manifest = toml::from_str(&fs::read_to_string(&path)?)?;
        edit(&mut manifest)?;
        fs::write(path, toml::to_string(&manifest)?)?;
        Ok(())
    }
}

fn relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!("expected a relative package path, got {}", path.display()).into());
    }
    Ok(())
}
