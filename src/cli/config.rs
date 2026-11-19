use std::{
    collections::BTreeMap,
    env,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

use r#override::Result;
use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Value};

use super::provider::{Fragment, Provider};

const INCLUDE: &str = "override.toml";

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Active {
    include: Vec<PathBuf>,
}

struct Store {
    home: PathBuf,
    providers: BTreeMap<String, PathBuf>,
    _lock: File,
}

pub fn home() -> Result<PathBuf> {
    cargo_config2::cargo_home_with_cwd(&env::current_dir()?)
        .ok_or_else(|| "Cargo home directory is unavailable".into())
}

impl Store {
    fn open() -> Result<Self> {
        let home = home()?;
        let root = home.join("override");
        fs::create_dir_all(&root)?;
        let lock = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("lock"))?;
        lock.lock()?;
        let active: Active = match fs::read_to_string(home.join(INCLUDE)) {
            Ok(data) => toml::from_str(&data)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Active::default(),
            Err(err) => return Err(err.into()),
        };
        let mut providers = BTreeMap::new();
        for file in active.include {
            let dir = file.parent().ok_or("invalid override include path")?;
            if dir.parent() != Some(Path::new("override"))
                || !dir
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("entry-"))
            {
                return Err(format!("unmanaged include path: {}", file.display()).into());
            }
            let (name, _) = identity(&file)?;
            if providers.insert(name.to_owned(), file).is_some() {
                return Err("duplicate provider in override.toml".into());
            }
        }
        Ok(Self {
            home,
            providers,
            _lock: lock,
        })
    }

    fn save(&self) -> Result<()> {
        let active = Active {
            include: self.providers.values().cloned().collect(),
        };
        write(&self.home.join(INCLUDE), &toml::to_string(&active)?)
    }

    fn fragment(&self, file: &Path) -> Result<Fragment> {
        Ok(toml::from_str(&fs::read_to_string(self.home.join(file))?)?)
    }
}

fn identity(file: &Path) -> Result<(&str, &str)> {
    file.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.split_once('@'))
        .ok_or_else(|| format!("invalid provider file: {}", file.display()).into())
}

pub fn enable(provider: Provider) -> Result<()> {
    let mut store = Store::open()?;
    let file = provider.prepare(&store.home)?;
    let file = file.strip_prefix(&store.home)?;
    let (name, _) = identity(file)?;
    let new = store.fragment(file)?;
    for (other, active) in &store.providers {
        if other == name {
            continue;
        }
        let existing = store.fragment(active)?;
        for (source, entries) in &new.patch {
            if let Some(package) = entries.keys().find(|key| {
                existing
                    .patch
                    .get(source)
                    .is_some_and(|entries| entries.contains_key(*key))
            }) {
                return Err(format!("{other} already replaces {package} from {source}").into());
            }
        }
    }
    new.resolve(&store.home.join(file))?;
    include(&store.home)?;
    if store.providers.insert(name.into(), file.into()).as_deref() != Some(file) {
        store.save()?;
    }
    Ok(())
}

pub fn disable(name: &str) -> Result<()> {
    let mut store = Store::open()?;
    store
        .providers
        .remove(name)
        .ok_or_else(|| format!("{name} is not enabled"))?;
    store.save()
}

pub fn list() -> Result<()> {
    let store = Store::open()?;
    for file in store.providers.values() {
        let (name, version) = identity(file)?;
        for (source, entries) in store.fragment(file)?.patch {
            for (package, entry) in entries {
                println!(
                    "{name}@{version} replaces {package}@{} from {source}",
                    entry.version
                );
            }
        }
    }
    Ok(())
}

fn include(home: &Path) -> Result<()> {
    let legacy = home.join("config");
    let path = if legacy.try_exists()? {
        legacy
    } else {
        home.join("config.toml")
    };
    let mut doc = match fs::read_to_string(&path) {
        Ok(text) => text.parse::<DocumentMut>()?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => DocumentMut::new(),
        Err(err) => return Err(err.into()),
    };
    let includes = doc
        .entry("include")
        .or_insert(toml_edit::value(Array::new()));
    let managed = |path: &str| home.join(path) == home.join(INCLUDE);
    let mut entry = InlineTable::new();
    entry.insert("path", INCLUDE.into());
    entry.insert("optional", true.into());
    match includes {
        Item::Value(Value::Array(array)) => {
            if array.iter().any(|value| {
                value
                    .as_str()
                    .or_else(|| value.as_inline_table()?.get("path")?.as_str())
                    .is_some_and(managed)
            }) {
                return Ok(());
            }
            array.push(Value::InlineTable(entry));
        }
        Item::ArrayOfTables(array) => {
            if array.iter().any(|table| {
                table
                    .get("path")
                    .and_then(Item::as_str)
                    .is_some_and(managed)
            }) {
                return Ok(());
            }
            array.push(entry.into_table());
        }
        _ => return Err("Cargo config include must be an array".into()),
    }
    write(&path, &doc.to_string())
}

fn write(path: &Path, contents: &str) -> Result<()> {
    let path = match fs::symlink_metadata(path) {
        Ok(_) => path.canonicalize()?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => path.to_owned(),
        Err(err) => return Err(err.into()),
    };
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    if path.try_exists()? {
        file.as_file()
            .set_permissions(fs::metadata(&path)?.permissions())?;
    }
    file.write_all(contents.as_bytes())?;
    file.persist(path)?;
    Ok(())
}
