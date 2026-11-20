use std::{env, fmt::Write, fs, path::Path, process::Command};

use r#override::{Manifest, Result, Sources, root};
use ra_ap_syntax::{
    AstNode, Edition, SourceFile, SyntaxKind, SyntaxNode, TextRange, ast,
    ast::{HasAttrs, HasModuleItem},
};

pub fn embed(sources: &mut Sources) -> Result<()> {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let sysroot = Command::new(&rustc).args(["--print", "sysroot"]).output()?;
    if !sysroot.status.success() {
        return Err(String::from_utf8(sysroot.stderr)?.into());
    }
    let library =
        Path::new(str::from_utf8(&sysroot.stdout)?.trim()).join("lib/rustlib/src/rust/library");
    let mut output = String::from("pub const SOURCES: &[(&str, span::Edition, &str)] = &[\n");
    for (krate, files) in [
        ("core", &["lib.rs", "macros/mod.rs", "panic.rs"][..]),
        ("alloc", &["lib.rs", "macros.rs"][..]),
        (
            "std",
            &[
                "lib.rs",
                "macros.rs",
                "panic.rs",
                "thread/mod.rs",
                "thread/local.rs",
            ][..],
        ),
    ] {
        let directory = library.join(krate);
        let manifest = directory.join("Cargo.toml");
        let manifest: Manifest = toml::from_str(
            &fs::read_to_string(&manifest)
                .map_err(|error| format!("{}: {error}", manifest.display()))?,
        )?;
        let edition: Edition = manifest["package"]["edition"]
            .as_str()
            .ok_or("missing standard library edition")?
            .parse()?;
        for file in files {
            let name = file.trim_end_matches(".rs").trim_end_matches("/mod");
            let module = if name == "lib" {
                krate.to_owned()
            } else {
                format!("{krate}::{}", name.replace('/', "::"))
            };
            let path = directory.join("src").join(file);
            let input = fs::read_to_string(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let parsed = SourceFile::parse(&input, edition);
            let source = declarations(parsed.tree().items())?;
            writeln!(
                output,
                "    ({module:?}, span::Edition::{edition:?}, {source:?}),"
            )?;
        }
    }
    output.push_str("];\n");
    fs::write(sources.path("src/stdlib.rs")?, output)?;
    sources.edit("src/lib.rs", |source| {
        source
            .select(root())?
            .mount_module("pub mod stdlib", "stdlib.rs")
    })
}

fn declarations(items: impl Iterator<Item = ast::Item>) -> Result<String> {
    let mut source = String::new();
    for item in items {
        let node = item.syntax();
        let text = match &item {
            ast::Item::Use(import)
                if import
                    .attrs()
                    .any(|attr| attr.simple_name().as_deref() == Some("prelude_import")) =>
            {
                continue;
            }
            ast::Item::MacroRules(_)
            | ast::Item::MacroDef(_)
            | ast::Item::Use(_)
            | ast::Item::ExternCrate(_) => header(node, node.text_range().end())?,
            ast::Item::Module(module) => {
                let Some(list) = module.item_list() else {
                    continue;
                };
                let contents = declarations(list.items())?;
                if contents.is_empty() {
                    continue;
                }
                format!(
                    "{}{{\n{contents}}}",
                    header(node, list.syntax().text_range().start())?
                )
            }
            _ => continue,
        };
        for attr in item.attrs().filter(|attr| {
            matches!(
                attr.simple_name().as_deref(),
                Some(
                    "cfg"
                        | "cfg_attr"
                        | "macro_export"
                        | "macro_use"
                        | "rustc_builtin_macro"
                        | "rustc_macro_transparency"
                )
            )
        }) {
            writeln!(source, "{attr}")?;
        }
        writeln!(source, "{text}")?;
    }
    Ok(source)
}

fn header(node: &SyntaxNode, end: ra_ap_syntax::TextSize) -> Result<String> {
    let start = node
        .children_with_tokens()
        .find(|element| {
            !element.kind().is_trivia()
                && !matches!(element.kind(), SyntaxKind::ATTR | SyntaxKind::DOC_COMMENT)
        })
        .ok_or("standard library declaration has no header")?
        .text_range()
        .start();
    Ok(node
        .text()
        .slice(TextRange::new(start, end) - node.text_range().start())
        .to_string())
}
