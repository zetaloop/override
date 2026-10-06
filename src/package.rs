use std::{collections::BTreeMap, env, fs, path::Path, process::Command, sync::Arc};

use cargo_config2::SourceConfigValue;
use cargo_metadata::{
    CargoOpt, Dependency, Metadata, MetadataCommand, PackageId, Target, TargetKind,
};

use crate::{Config, Edition, Result};

#[derive(Clone, Debug)]
pub struct Package {
    pub(crate) graph: Arc<Metadata>,
    pub(crate) index: usize,
    pub(crate) alias: Option<String>,
    pub(crate) config: Option<Arc<BTreeMap<String, SourceConfigValue>>>,
}

impl Package {
    pub fn current() -> Result<Self> {
        Self::load(env::var_os("CARGO_MANIFEST_PATH").ok_or("CARGO_MANIFEST_PATH is unavailable")?)
    }

    pub fn load(manifest: impl AsRef<Path>) -> Result<Self> {
        let manifest = fs::canonicalize(manifest)?;
        let context = manifest.parent().ok_or("manifest has no parent")?;
        let graph = MetadataCommand::new()
            .manifest_path(&manifest)
            .current_dir(context)
            .features(CargoOpt::AllFeatures)
            .exec()?;
        let index = graph
            .packages
            .iter()
            .position(|package| {
                fs::canonicalize(&package.manifest_path).is_ok_and(|path| path == manifest)
            })
            .ok_or_else(|| format!("Cargo metadata has no package at {}", manifest.display()))?;
        Ok(Self {
            graph: Arc::new(graph),
            index,
            alias: None,
            config: Some(Arc::new(Config::load_with_cwd(context)?.source)),
        })
    }

    pub fn from_metadata(graph: Metadata, id: &PackageId) -> Result<Self> {
        let index = graph
            .packages
            .iter()
            .position(|package| package.id == *id)
            .ok_or_else(|| format!("Cargo metadata has no package `{id}`"))?;
        Ok(Self {
            graph: Arc::new(graph),
            index,
            alias: None,
            config: None,
        })
    }

    pub fn config(mut self, config: Config) -> Self {
        self.config = Some(Arc::new(config.source));
        self
    }

    pub fn data(&self) -> &cargo_metadata::Package {
        &self.graph.packages[self.index]
    }

    pub fn directory(&self) -> &Path {
        self.data()
            .manifest_path
            .parent()
            .expect("package manifest has a directory")
            .as_std_path()
    }

    pub fn edition(&self) -> Result<Edition> {
        edition(self.data().edition)
    }

    pub fn library(&self) -> Result<&Target> {
        let targets = self
            .data()
            .targets
            .iter()
            .filter(|target| library(target))
            .collect::<Vec<_>>();
        match targets.as_slice() {
            [target] => Ok(target),
            _ => Err(format!(
                "package `{}` has no unique library target",
                self.data().name
            )
            .into()),
        }
    }

    pub fn build_script(&self) -> Result<&Target> {
        self.data()
            .targets
            .iter()
            .find(|target| target.is_custom_build())
            .ok_or_else(|| format!("package `{}` has no build script", self.data().name).into())
    }

    pub fn target(&self, kind: TargetKind, name: &str) -> Result<&Target> {
        self.data()
            .targets
            .iter()
            .find(|target| target.is_kind(kind.clone()) && target.name == name)
            .ok_or_else(|| {
                format!(
                    "package `{}` has no {kind} target `{name}`",
                    self.data().name
                )
                .into()
            })
    }

    pub fn dependency(&self, alias: &str) -> Result<Self> {
        let mut packages = Vec::new();
        for dependency in self
            .data()
            .dependencies
            .iter()
            .filter(|dependency| dependency_name(dependency) == alias)
        {
            let package = self.resolve_dependency(dependency)?;
            if !packages
                .iter()
                .any(|candidate: &Self| candidate.data().id == package.data().id)
            {
                packages.push(package);
            }
        }
        match packages.as_slice() {
            [package] => Ok(package.clone()),
            [] => Err(format!("package `{}` has no dependency `{alias}`", self.data().name).into()),
            _ => Err(format!(
                "dependency `{alias}` selects multiple packages; select a declaration with resolve"
            )
            .into()),
        }
    }

    pub fn resolve(&self, dependency: &Dependency) -> Result<Self> {
        if !self.data().dependencies.contains(dependency) {
            return Err("dependency belongs to a different package".into());
        }
        self.resolve_dependency(dependency)
    }

    pub(crate) fn resolve_dependency(&self, dependency: &Dependency) -> Result<Self> {
        self.resolved_dependency(dependency)?.ok_or_else(|| {
            format!(
                "dependency `{}` is absent from the selected Cargo graph",
                dependency_name(dependency)
            )
            .into()
        })
    }

    pub(crate) fn resolved_dependency(&self, dependency: &Dependency) -> Result<Option<Self>> {
        let resolve = self
            .graph
            .resolve
            .as_ref()
            .ok_or("Cargo metadata has no dependency graph")?;
        let node = resolve
            .nodes
            .iter()
            .find(|node| node.id == self.data().id)
            .ok_or("package has no dependency node")?;
        let mut candidates = Vec::new();
        for edge in &node.deps {
            let Some(index) = self
                .graph
                .packages
                .iter()
                .position(|package| package.id == edge.pkg)
            else {
                return Err("dependency package is missing from Cargo metadata".into());
            };
            let package = &self.graph.packages[index];
            let name = dependency
                .rename
                .as_ref()
                .map(|alias| alias.replace('-', "_"))
                .unwrap_or_else(|| {
                    package
                        .targets
                        .iter()
                        .find(|target| library(target))
                        .map_or_else(
                            || dependency.name.replace('-', "_"),
                            |target| target.name.clone(),
                        )
                });
            if package.name != dependency.name
                || edge.name != name
                || !dependency.req.matches(&package.version)
                || !edge
                    .dep_kinds
                    .iter()
                    .any(|kind| kind.kind == dependency.kind && kind.target == dependency.target)
            {
                continue;
            }
            if let Some(path) = &dependency.path
                && fs::canonicalize(path)?
                    != fs::canonicalize(
                        package
                            .manifest_path
                            .parent()
                            .ok_or("dependency manifest has no directory")?,
                    )?
            {
                continue;
            }
            if !candidates.contains(&index) {
                candidates.push(index);
            }
        }
        match candidates.as_slice() {
            [index] => Ok(Some(Self {
                graph: self.graph.clone(),
                index: *index,
                alias: Some(dependency_name(dependency).to_owned()),
                config: self.config.clone(),
            })),
            [] => Ok(None),
            _ => Err(format!(
                "dependency `{}` has multiple resolved packages",
                dependency_name(dependency)
            )
            .into()),
        }
    }

    pub(crate) fn manifest(&self) -> Result<toml::Table> {
        Ok(toml::from_str(&fs::read_to_string(
            &self.data().manifest_path,
        )?)?)
    }

    pub(crate) fn workspace_manifest(&self) -> Result<std::path::PathBuf> {
        let text = output(&mut self.command(&[
            "locate-project",
            "--workspace",
            "--message-format",
            "plain",
        ]))?;
        Ok(text.trim().into())
    }

    pub(crate) fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        command
            .current_dir(self.directory())
            .args(arguments)
            .arg("--manifest-path")
            .arg(&self.data().manifest_path);
        command
    }
}

pub(crate) fn output(command: &mut Command) -> Result<String> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "{command:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub(crate) fn dependency_name(dependency: &Dependency) -> &str {
    dependency.rename.as_deref().unwrap_or(&dependency.name)
}

pub(crate) fn edition(edition: cargo_metadata::Edition) -> Result<Edition> {
    edition
        .as_str()
        .parse()
        .map_err(|error| format!("invalid edition {edition}: {error}").into())
}

fn library(target: &Target) -> bool {
    target.kind.iter().any(|kind| {
        matches!(
            kind,
            TargetKind::Lib
                | TargetKind::RLib
                | TargetKind::DyLib
                | TargetKind::CDyLib
                | TargetKind::StaticLib
                | TargetKind::ProcMacro
        )
    })
}
