use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    fs,
};

use cargo_metadata::{Dependency, DependencyKind, Target};
use toml::Value;

use crate::{Package, Result, package::dependency_name};

impl Package {
    pub fn check_dependencies(
        &self,
        upstream: &Package,
        kind: DependencyKind,
        replacements: &[(&str, Package)],
    ) -> Result<()> {
        let aliases = self.replacement_aliases(upstream, replacements)?;
        let mut differences = Vec::new();
        for expected in upstream
            .data()
            .dependencies
            .iter()
            .filter(|dependency| dependency.kind == kind)
        {
            let alias = dependency_name(expected);
            let actual_alias = aliases.get(alias).map_or(alias, String::as_str);
            let label = format!(
                "{kind} dependency `{alias}`{}",
                expected
                    .target
                    .as_ref()
                    .map_or_else(String::new, |target| format!(" for {target}"))
            );
            let actual = self
                .data()
                .dependencies
                .iter()
                .filter(|dependency| {
                    dependency.kind == kind
                        && dependency_name(dependency) == actual_alias
                        && dependency.target == expected.target
                })
                .collect::<Vec<_>>();
            let [actual] = actual.as_slice() else {
                differences.push(format!("{label}: missing or ambiguous declaration"));
                continue;
            };
            compare(
                &format!("{label} optional"),
                &expected.optional,
                &actual.optional,
                &mut differences,
            );
            compare(
                &format!("{label} default-features"),
                &expected.uses_default_features,
                &actual.uses_default_features,
                &mut differences,
            );
            compare(
                &format!("{label} features"),
                &set(&expected.features),
                &set(&actual.features),
                &mut differences,
            );
            if let Some((_, replacement)) = replacements.iter().find(|(name, _)| *name == alias) {
                let package = self.resolve_dependency(actual)?;
                compare(
                    &format!("{label} package"),
                    &replacement.data().id,
                    &package.data().id,
                    &mut differences,
                );
            } else {
                compare(
                    &format!("{label} name"),
                    &expected.name,
                    &actual.name,
                    &mut differences,
                );
                let expected_package = upstream.resolved_dependency(expected)?;
                let actual_package = self.resolved_dependency(actual)?;
                if let (Some(expected), Some(actual)) = (expected_package, actual_package) {
                    compare(
                        &format!("{label} package"),
                        &expected.data().id,
                        &actual.data().id,
                        &mut differences,
                    );
                } else {
                    compare(
                        &format!("{label} version"),
                        &expected.req,
                        &actual.req,
                        &mut differences,
                    );
                    if !same_source(upstream, expected, self, actual)? {
                        differences.push(format!("{label}: different source"));
                    }
                }
            }
        }
        finish(&self.data().name, &upstream.data().name, differences)
    }

    pub fn check_features(
        &self,
        upstream: &Package,
        replacements: &[(&str, Package)],
    ) -> Result<()> {
        let aliases = self.replacement_aliases(upstream, replacements)?;
        let inherited = upstream
            .data()
            .dependencies
            .iter()
            .map(|dependency| {
                let name = dependency_name(dependency);
                aliases.get(name).map_or(name, String::as_str)
            })
            .collect::<BTreeSet<_>>();
        let mut differences = Vec::new();
        for (feature, expected) in &upstream.data().features {
            let Some(actual) = self.data().features.get(feature) else {
                differences.push(format!("feature `{feature}` is missing"));
                continue;
            };
            let expected = expected
                .iter()
                .map(|member| feature_member(member, &aliases))
                .collect::<BTreeSet<_>>();
            let actual = actual.iter().cloned().collect::<BTreeSet<_>>();
            for missing in expected.difference(&actual) {
                differences.push(format!("feature `{feature}` is missing `{missing}`"));
            }
            for extra in actual.difference(&expected) {
                let own = if let Some(dependency) = extra.strip_prefix("dep:") {
                    !inherited.contains(dependency)
                } else if let Some((dependency, _)) = extra.split_once('/') {
                    !inherited.contains(dependency.trim_end_matches('?'))
                } else {
                    !upstream.data().features.contains_key(extra)
                };
                if !own {
                    differences.push(format!(
                        "feature `{feature}` adds upstream member `{extra}`"
                    ));
                }
            }
        }
        finish(&self.data().name, &upstream.data().name, differences)
    }

    pub fn check_lints(&self, upstream: &Package) -> Result<()> {
        let expected = upstream.lints()?;
        let actual = self.lints()?;
        let mut differences = Vec::new();
        for (tool, rules) in expected {
            let expected = rules.as_table().ok_or("lint group is not a table")?;
            let actual = actual.get(&tool).and_then(Value::as_table);
            for (name, rule) in expected {
                let label = format!("lint {tool}.{name}");
                match actual.and_then(|rules| rules.get(name)) {
                    Some(value) => compare(&label, &lint(rule)?, &lint(value)?, &mut differences),
                    None => differences.push(format!("{label} is missing")),
                }
            }
        }
        finish(&self.data().name, &upstream.data().name, differences)
    }

    pub(crate) fn replacement_aliases(
        &self,
        upstream: &Package,
        replacements: &[(&str, Package)],
    ) -> Result<BTreeMap<String, String>> {
        let mut aliases = BTreeMap::new();
        for (name, replacement) in replacements {
            if !upstream
                .data()
                .dependencies
                .iter()
                .any(|dependency| dependency_name(dependency) == *name)
            {
                return Err(format!("upstream has no dependency `{name}`").into());
            }
            let names = self
                .data()
                .dependencies
                .iter()
                .filter(|dependency| {
                    replacement
                        .alias
                        .as_deref()
                        .is_none_or(|alias| alias == dependency_name(dependency))
                })
                .map(|dependency| {
                    Ok((
                        dependency_name(dependency),
                        self.resolved_dependency(dependency)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .filter_map(|(name, package)| {
                    package
                        .filter(|package| package.data().id == replacement.data().id)
                        .map(|_| name)
                })
                .collect::<BTreeSet<_>>();
            if names.len() != 1 {
                return Err(
                    format!("replacement for `{name}` has no unique bridge dependency").into(),
                );
            }
            let alias = names.into_iter().next().ok_or("replacement has no alias")?;
            if aliases
                .insert((*name).to_owned(), alias.to_owned())
                .is_some()
            {
                return Err(format!("duplicate replacement for `{name}`").into());
            }
        }
        Ok(aliases)
    }

    fn lints(&self) -> Result<toml::Table> {
        let manifest = self.manifest()?;
        let Some(lints) = manifest.get("lints").and_then(Value::as_table) else {
            return Ok(toml::Table::new());
        };
        if lints.get("workspace").and_then(Value::as_bool) == Some(true) {
            let workspace = self.workspace_manifest()?;
            println!("cargo::rerun-if-changed={}", workspace.display());
            let manifest: toml::Table = toml::from_str(&fs::read_to_string(workspace)?)?;
            return Ok(manifest
                .get("workspace")
                .and_then(|workspace| workspace.get("lints"))
                .and_then(Value::as_table)
                .cloned()
                .unwrap_or_default());
        }
        Ok(lints.clone())
    }
}

// Target names and test discovery belong to the bridge.
pub fn check_target(upstream: &Target, bridge: &Target) -> Result<()> {
    let mut differences = Vec::new();
    compare(
        "edition",
        &upstream.edition,
        &bridge.edition,
        &mut differences,
    );
    compare(
        "crate types",
        &upstream.crate_types.iter().collect::<BTreeSet<_>>(),
        &bridge.crate_types.iter().collect::<BTreeSet<_>>(),
        &mut differences,
    );
    compare(
        "required features",
        &set(&upstream.required_features),
        &set(&bridge.required_features),
        &mut differences,
    );
    finish(&bridge.name, &upstream.name, differences)
}

fn same_source(
    upstream: &Package,
    expected: &Dependency,
    bridge: &Package,
    actual: &Dependency,
) -> Result<bool> {
    if expected.source == actual.source && expected.path == actual.path {
        return Ok(true);
    }
    if let (Some(expected), Some(actual)) = (&expected.path, &actual.path) {
        return Ok(fs::canonicalize(expected)? == fs::canonicalize(actual)?);
    }
    if expected.path.is_some()
        && upstream.data().source.is_some()
        && let Some(package) = bridge.resolved_dependency(actual)?
    {
        return Ok(package.data().source == upstream.data().source);
    }
    Ok(false)
}

fn feature_member(member: &str, aliases: &BTreeMap<String, String>) -> String {
    if let Some(dependency) = member.strip_prefix("dep:") {
        return format!(
            "dep:{}",
            aliases.get(dependency).map_or(dependency, String::as_str)
        );
    }
    if let Some((dependency, feature)) = member.split_once('/') {
        let name = dependency.trim_end_matches('?');
        let weak = if dependency.ends_with('?') { "?" } else { "" };
        return format!(
            "{}{weak}/{feature}",
            aliases.get(name).map_or(name, String::as_str)
        );
    }
    member.to_owned()
}

fn lint(value: &Value) -> Result<Value> {
    let mut rule = match value {
        Value::String(level) => {
            toml::Table::from_iter([("level".to_owned(), Value::String(level.clone()))])
        }
        Value::Table(rule) => rule.clone(),
        _ => return Err("invalid lint configuration".into()),
    };
    rule.entry("priority").or_insert(Value::Integer(0));
    Ok(Value::Table(rule))
}

fn set(values: &[String]) -> BTreeSet<&str> {
    values.iter().map(String::as_str).collect()
}

fn compare<T: PartialEq + Debug>(
    label: &str,
    expected: &T,
    actual: &T,
    differences: &mut Vec<String>,
) {
    if expected != actual {
        differences.push(format!("{label}: expected {expected:?}, got {actual:?}"));
    }
}

fn finish(bridge: &str, upstream: &str, differences: Vec<String>) -> Result<()> {
    if differences.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "`{bridge}` differs from `{upstream}`:\n{}",
            differences.join("\n")
        )
        .into())
    }
}
