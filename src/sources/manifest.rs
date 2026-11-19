use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

use cargo_metadata::{DependencyKind, camino::Utf8PathBuf, cargo_platform::Platform};
use toml::Value;

use crate::{
    Manifest, Package, Result,
    package::{dependency_name, output},
};

pub(super) fn prepare(
    package: &Package,
    files: &BTreeMap<PathBuf, PathBuf>,
) -> Result<Option<String>> {
    let data = package.data();
    let source = data.manifest_path.parent().unwrap();
    let original: Manifest = toml::from_str(&fs::read_to_string(&data.manifest_path)?)?;
    let mut manifest = original.clone();
    let mut workspace = manifest
        .remove("workspace")
        .map(|table| (source.to_owned(), table));
    let section = if manifest.contains_key("package") {
        "package"
    } else {
        "project"
    };
    let member = manifest
        .get_mut(section)
        .and_then(Value::as_table_mut)
        .ok_or("manifest has no package table")?;
    let explicit = member.remove("workspace");

    let mut inherit = |section: &str, key: Option<&str>| -> Result<(Utf8PathBuf, Value)> {
        if workspace.is_none() {
            let path = if package.graph.workspace_members.contains(&data.id) {
                package.graph.workspace_root.join("Cargo.toml")
            } else if let Some(path) = explicit.as_ref().and_then(Value::as_str) {
                source.join(path).join("Cargo.toml")
            } else {
                let path = output(&mut package.command(&[
                    "locate-project",
                    "--workspace",
                    "--message-format",
                    "plain",
                ]))?;
                Utf8PathBuf::from(path.trim_end())
            };
            let mut root: Manifest = toml::from_str(&fs::read_to_string(&path)?)?;
            workspace = Some((
                path.parent().unwrap().to_owned(),
                root.remove("workspace")
                    .ok_or("manifest has no workspace table")?,
            ));
        }
        let (dir, root) = workspace.as_ref().unwrap();
        let table = root.get(section);
        let value = match key {
            Some(key) => table.and_then(|table| table.get(key)),
            None => table,
        };
        Ok((
            dir.clone(),
            value
                .ok_or_else(|| {
                    format!(
                        "workspace.{section} has no inherited value for {}",
                        key.unwrap_or(section)
                    )
                })?
                .clone(),
        ))
    };

    for (key, value) in original[section].as_table().unwrap() {
        let effective = match key.as_str() {
            "readme" if value.is_str() || inherited(value) => Some(
                data.readme()
                    .map_or_else(|| false.into(), |p| packaged_file(p.as_std_path(), files)),
            ),
            "license-file" if value.is_str() || inherited(value) => data
                .license_file()
                .map(|p| packaged_file(p.as_std_path(), files)),
            _ if !inherited(value) => continue,
            "edition" => Some(data.edition.as_str().into()),
            "version" => Some(data.version.to_string().into()),
            "authors" => Some(data.authors.clone().into()),
            "categories" => Some(data.categories.clone().into()),
            "keywords" => Some(data.keywords.clone().into()),
            "description" => data.description.as_deref().map(Value::from),
            "documentation" => data.documentation.as_deref().map(Value::from),
            "homepage" => data.homepage.as_deref().map(Value::from),
            "license" => data.license.as_deref().map(Value::from),
            "repository" => data.repository.as_deref().map(Value::from),
            "rust-version" => data.rust_version.as_ref().map(|v| v.to_string().into()),
            "publish" => Some(match &data.publish {
                Some(names) if names.is_empty() => false.into(),
                Some(names) => names.clone().into(),
                None => true.into(),
            }),
            "include" | "exclude" => Some(inherit("package", Some(key))?.1),
            _ => continue,
        };
        match effective {
            Some(value) => member.insert(key.clone(), value),
            None => member.remove(key),
        };
    }
    if let Some(lints) = manifest.get_mut("lints")
        && inherited(lints)
    {
        *lints = inherit("lints", None)?.1;
    }

    let mut dependencies = |table: &mut Manifest, target: Option<&Platform>| -> Result<()> {
        for (section, entries) in table {
            let kind = match section.as_str() {
                "dependencies" => DependencyKind::Normal,
                "build-dependencies" | "build_dependencies" => DependencyKind::Build,
                "dev-dependencies" | "dev_dependencies" => DependencyKind::Development,
                _ => continue,
            };
            for (alias, entry) in entries.as_table_mut().into_iter().flatten() {
                let base = if inherited(entry) {
                    let (dir, template) = inherit("dependencies", Some(alias))?;
                    let dep = data
                        .dependencies
                        .iter()
                        .find(|dep| {
                            dependency_name(dep) == alias
                                && dep.kind == kind
                                && dep.target.as_ref() == target
                        })
                        .ok_or_else(|| {
                            format!("Cargo metadata has no declaration for `{alias}` in {section}")
                        })?;
                    let mut detail = match template {
                        Value::String(version) => toml::toml! { version = (version) },
                        Value::Table(table) => table,
                        _ => return Err(format!("invalid workspace dependency `{alias}`").into()),
                    };
                    if !dep.features.is_empty() {
                        detail.insert("features".into(), dep.features.clone().into());
                    }
                    detail.remove("default_features");
                    detail.remove("default-features");
                    if !dep.uses_default_features {
                        detail.insert("default-features".into(), false.into());
                    }
                    if dep.optional {
                        detail.insert("optional".into(), true.into());
                    }
                    *entry = detail.into();
                    dir
                } else {
                    source.to_owned()
                };
                if let Some(path) = entry.get_mut("path")
                    && let Some(relative) = path.as_str()
                {
                    *path = base.join(relative).into_string().into();
                }
            }
        }
        Ok(())
    };
    dependencies(&mut manifest, None)?;
    if let Some(targets) = manifest.get_mut("target").and_then(Value::as_table_mut) {
        for (name, table) in targets {
            if let Some(table) = table.as_table_mut() {
                dependencies(table, Some(&name.parse()?))?;
            }
        }
    }
    Ok((manifest != original)
        .then(|| toml::to_string(&manifest))
        .transpose()?)
}

fn inherited(value: &Value) -> bool {
    value.get("workspace").and_then(Value::as_bool) == Some(true)
}

fn packaged_file(file: &Path, files: &BTreeMap<PathBuf, PathBuf>) -> Value {
    let mut normalized = PathBuf::new();
    for part in file.components() {
        match part {
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(part),
        }
    }
    let name = files
        .iter()
        .find_map(|(name, path)| (path == &normalized).then_some(name.as_path()))
        .unwrap_or_else(|| Path::new(file.file_name().unwrap()));
    name.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
        .into()
}
