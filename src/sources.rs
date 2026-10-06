use std::{
    collections::BTreeSet,
    fs, io,
    path::{Component, Path, PathBuf},
};

use cargo_metadata::Target;
use flate2::read::GzDecoder;
use ra_ap_syntax::{AstNode, ast, syntax_editor::SyntaxEditor};

use crate::{Edition, Package, Result, Source, fragment, package::edition};

const OWNER: &str = "Source directory prepared by override.\n";
const MARKER: &str = ".override-source";

pub struct Sources {
    package: Package,
    directory: PathBuf,
    edition: Edition,
}

pub struct Entry {
    pub path: PathBuf,
    /// `include!` rejects inner attributes; these belong in the bridge's crate root.
    pub attributes: String,
    edition: Edition,
}

impl Package {
    pub fn prepare(&self, destination: impl AsRef<Path>) -> Result<Sources> {
        let source = fs::canonicalize(self.directory())?;
        let mut files = BTreeSet::new();
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
                        .map(PathBuf::from),
                );
                println!("cargo::rerun-if-changed={}", checksum.display());
            } else {
                let config = cargo_config2::Config::load_with_cwd(&self.context)?;
                let mut configured = config.source.iter().find_map(|(name, source)| {
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
                    configured = config.source.get(name);
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
                println!("cargo::rerun-if-changed={}", path.display());
            }
        } else {
            let listing = self.command(&[
                "package",
                "--list",
                "--allow-dirty",
                "--package",
                self.data().name.as_ref(),
            ])?;
            files.extend(listing.lines().map(PathBuf::from));
            if source.join(".cargo_vcs_info.json").is_file() {
                files.insert(".cargo_vcs_info.json".into());
            }
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
        let external = [self.data().readme(), self.data().license_file()];
        for relative in files {
            relative_path(&relative)?;
            let mut original = source.join(&relative);
            if original.starts_with(&destination) {
                continue;
            }
            if relative == Path::new("Cargo.toml.orig") && !original.try_exists()? {
                original = self.data().manifest_path.as_std_path().to_owned();
            }
            if !original.try_exists()? {
                if let Some(path) = external
                    .iter()
                    .flatten()
                    .find(|path| Path::new(path.file_name().unwrap_or_default()) == relative)
                {
                    original = path.as_std_path().to_owned();
                } else if relative == Path::new("Cargo.lock")
                    || relative == Path::new(".cargo_vcs_info.json")
                {
                    continue;
                }
            }
            let output = destination.join(&relative);
            fs::create_dir_all(output.parent().ok_or("source file has no parent")?)?;
            let mut input = fs::File::open(&original)?;
            let mut output = fs::File::create(&output)?;
            io::copy(&mut input, &mut output)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                output.set_permissions(fs::Permissions::from_mode(
                    input.metadata()?.permissions().mode() | 0o200,
                ))?;
            }
            println!("cargo::rerun-if-changed={}", original.display());
        }
        if !destination.starts_with(&source) && self.data().source.is_none() {
            println!("cargo::rerun-if-changed={}", source.display());
        }
        println!("cargo::rerun-if-changed={}", self.data().manifest_path);
        println!(
            "cargo::rerun-if-changed={}",
            self.graph.workspace_root.join("Cargo.toml")
        );
        println!(
            "cargo::rerun-if-changed={}",
            self.graph.workspace_root.join("Cargo.lock")
        );
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

    pub fn include(&mut self, target: &Target) -> Result<Entry> {
        if !self.package.data().targets.contains(target) {
            return Err("target belongs to a different package".into());
        }
        let relative = target
            .src_path
            .as_std_path()
            .strip_prefix(self.package.directory())?;
        let path = self.path(relative)?;
        let edition = edition(target.edition)?;
        let original = fs::read_to_string(&path)?;
        let source = fragment::file(&original, edition)?;
        let (editor, root) = SyntaxEditor::new(source);
        let mut attributes = String::new();
        for attribute in root
            .children()
            .filter_map(ast::AnyAttr::cast)
            .filter(|attribute| attribute.kind() == ast::AttrKind::Inner)
        {
            attributes.push_str(&attribute.syntax().to_string());
            attributes.push('\n');
            editor.delete(attribute.syntax());
        }
        if let Some(token) = root
            .first_token()
            .filter(|token| token.kind() == ra_ap_syntax::SyntaxKind::SHEBANG)
        {
            editor.delete(token);
        }
        let body = editor.finish().new_root().to_string();
        if body != original {
            fs::write(&path, body)?;
        }
        Ok(Entry {
            path,
            attributes,
            edition,
        })
    }
}

impl Entry {
    pub fn emit(&self, name: &str) -> Result<()> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err("invalid environment variable name".into());
        }
        let path = self.path.to_str().ok_or("entry path is not UTF-8")?;
        if path.contains(['\n', '\r']) {
            return Err("entry path contains a line break".into());
        }
        println!("cargo::rustc-env={name}={path}");
        Ok(())
    }

    pub fn export(&self, name: &str, destination: impl AsRef<Path>) -> Result<()> {
        fragment::name(name, self.edition)?;
        let path = self.path.to_str().ok_or("entry path is not UTF-8")?;
        fs::write(
            destination,
            format!(
                "#[macro_export]\nmacro_rules! {name} {{\n    () => {{ include!({path:?}); }};\n}}\n"
            ),
        )?;
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
