use std::collections::HashSet;

use mbe::{DeclarativeMacro, MacroCallStyle, MatchedArmIndex};
use ra_ap_syntax::{
    AstNode, Edition, SyntaxKind, SyntaxNode, SyntaxToken, TextRange, ast,
    ast::{HasAttrs, HasName},
};
use span::{
    EditionedFileId, FileId, ROOT_ERASED_FILE_AST_ID, Span, SpanAnchor, SpanMap, SyntaxContext,
};
use syntax_bridge::{DocCommentDesugarMode, SpanMapper, TopEntryPoint};

use crate::{
    Result, Source, fragment, resolve,
    source::Location,
    symbol::{self, Binding, Namespace},
};

mod builtin;
mod project;
mod scope;
pub(crate) use scope::Scope;

const INPUT: u32 = 0;
const DEFINITION: u32 = 1;
const GENERATED: u32 = 2;

struct Origin<'a> {
    file: u32,
    edition: Edition,
    parent: Option<&'a SpanMap>,
}

impl SpanMapper for Origin<'_> {
    fn span_for(&self, range: TextRange) -> Span {
        Span {
            range,
            anchor: SpanAnchor {
                file_id: EditionedFileId::new(FileId::from_raw(self.file), self.edition),
                ast_id: ROOT_ERASED_FILE_AST_ID,
            },
            ctx: self.parent.map_or_else(
                || SyntaxContext::root(self.edition),
                |p| p.span_at(range.start()).ctx,
            ),
        }
    }
}

enum Rule {
    Declarative(DeclarativeMacro),
    Builtin(String),
}

struct Expansion {
    root: SyntaxNode,
    map: SpanMap,
    arm: MatchedArmIndex,
}

struct View {
    parent: usize,
    depth: usize,
    site: Location,
    definition: Location,
    input: ast::TokenTree,
    rule: Rule,
    expansion: Expansion,
}

#[derive(Default)]
pub(crate) struct Macros {
    db: salsa::DatabaseImpl,
    definitions: Vec<Location>,
    views: Vec<View>,
    recursion_limit: usize,
}

fn attribute(node: &SyntaxNode, name: &str) -> Option<ast::Attr> {
    node.children()
        .filter_map(ast::Attr::cast)
        .find(|a| a.simple_name().as_deref() == Some(name))
}

fn entry(call: &SyntaxNode) -> TopEntryPoint {
    let parent = call.parent().map(|n| {
        if n.kind() == SyntaxKind::MACRO_EXPR {
            n.parent().unwrap_or(n)
        } else {
            n
        }
    });
    match parent.as_ref().map(SyntaxNode::kind) {
        Some(SyntaxKind::EXPR_STMT | SyntaxKind::STMT_LIST | SyntaxKind::MACRO_STMTS) => {
            TopEntryPoint::MacroStmts
        }
        _ => TopEntryPoint::Expr,
    }
}

fn transcribe(
    db: &salsa::DatabaseImpl,
    rule: &Rule,
    args: &tt::TopSubtree,
    call: &SyntaxNode,
    site: Span,
) -> Result<Expansion> {
    let (output, arm) = match rule {
        Rule::Declarative(rule) => rule
            .expand(db, args, |_| (), MacroCallStyle::FnLike, site)
            .result()
            .map_err(|e| e.to_string())?,
        Rule::Builtin(name) => (builtin::flow(db, name, args, site)?, None),
    };
    let (parsed, map) =
        syntax_bridge::token_tree_to_syntax_node(&output, entry(call), &mut |ctx| ctx.edition(db));
    let errors = parsed.errors();
    if !errors.is_empty() {
        return Err(format!("invalid macro expansion: {errors:?}").into());
    }
    Ok(Expansion {
        root: parsed.syntax_node(),
        map,
        arm,
    })
}

impl Macros {
    fn load(&mut self, source: &Source) -> Result<()> {
        let roots = source.roots().collect::<Vec<_>>();
        self.recursion_limit = roots
            .iter()
            .find(|r| r.module.is_empty())
            .and_then(|r| attribute(&r.root, "recursion_limit"))
            .and_then(|a| a.syntax().descendants().find_map(ast::Literal::cast))
            .map(|lit| lit.to_string().trim_matches('"').parse::<usize>())
            .transpose()?
            .unwrap_or(128);
        let mut roots = roots;
        for &(module, edition, text) in mbe::stdlib::SOURCES {
            let mut root = Location::root(fragment::file(text, edition)?, edition);
            let (krate, path) = module.split_once("::").unwrap_or((module, ""));
            root.krate = krate.into();
            root.module = path.into();
            roots.push(root);
        }
        for root in roots {
            self.definitions.extend(
                root.node
                    .descendants()
                    .filter(|n| n == &root.node || ast::Item::can_cast(n.kind()))
                    .map(|n| root.at(n)),
            );
        }
        Ok(())
    }

    fn global(&self, site: &Location, name: &str) -> Result<Binding> {
        let matches = symbol::global(
            &self.definitions,
            site,
            name,
            Some(Namespace::Macro),
            &mut HashSet::new(),
        );
        Self::one(matches, name)
    }

    fn one(matches: Vec<Binding>, name: &str) -> Result<Binding> {
        match matches.as_slice() {
            [b] if b.unresolved.is_none() => Ok(b.clone()),
            [b] if let Some(target) = &b.unresolved
                && target != name =>
            {
                Err(
                    format!("`{name}!` resolves to `{target}`, whose definition is unavailable")
                        .into(),
                )
            }
            [] | [_] => Err(format!("definition of `{name}!` is unavailable").into()),
            _ => Err(format!("definition of `{name}!` is ambiguous").into()),
        }
    }

    fn origin(&self, mut view: usize, mut range: TextRange) -> Option<&Location> {
        while view != 0 {
            let v = &self.views[view - 1];
            let span = v.expansion.map.span_at(range.start());
            if span.anchor.file_id.file_id().index() == DEFINITION {
                return Some(&v.definition);
            }
            if v.parent == 0 {
                return Some(&v.site);
            }
            range = if span.anchor.file_id.file_id().index() == INPUT {
                span.range
            } else {
                v.site.node.text_range()
            };
            view = v.parent;
        }
        None
    }

    pub fn parents(&self, view: usize) -> impl Iterator<Item = &Location> + Clone {
        std::iter::successors(view.checked_sub(1), |&i| {
            self.views[i].parent.checked_sub(1)
        })
        .map(|i| &self.views[i].site)
    }

    pub fn source_range(&self, mut view: usize, mut range: TextRange) -> Option<TextRange> {
        while let Some(i) = view.checked_sub(1) {
            let v = &self.views[i];
            let span = v.expansion.map.span_at(range.start());
            if span.anchor.file_id.file_id().index() != INPUT {
                return None;
            }
            range = span.range;
            view = v.parent;
        }
        Some(range)
    }

    pub fn bound<'s>(
        &self,
        source: &'s Source,
        mut view: usize,
        at: &Location,
        mut range: TextRange,
    ) -> Option<&'s resolve::method::Target> {
        let mut site = at;
        while let Some(i) = view.checked_sub(1) {
            let v = &self.views[i];
            let span = v.expansion.map.span_at(range.start());
            match span.anchor.file_id.file_id().index() {
                INPUT => {
                    site = &v.site;
                    view = v.parent;
                    range = span.range;
                }
                DEFINITION => return source.bound(&v.definition, span.range),
                _ => return None,
            }
        }
        source.bound(site, range)
    }

    fn resolve(
        &self,
        source: &Source,
        parent: usize,
        at: &Location,
        path: &ast::Path,
    ) -> Result<Binding> {
        let name = resolve::path_name(path);
        let origin = self
            .origin(parent, path.syntax().text_range())
            .unwrap_or(at);
        if let Some(path) = name.strip_prefix("$crate::") {
            let name = if origin.krate.is_empty() {
                path.to_owned()
            } else {
                format!("{}::{path}", origin.krate)
            };
            return self.global(origin, &name);
        }
        let (matches, site) = symbol::scoped(
            &self.definitions,
            at,
            self.parents(parent),
            &name,
            Namespace::Macro,
        );
        if matches
            .iter()
            .any(|b| b.unresolved.is_none() || !b.imports.is_empty())
        {
            return Self::one(matches, &name);
        }
        if !name.contains("::")
            && !site
                .ancestors()
                .iter()
                .any(|n| attribute(n, "no_implicit_prelude").is_some())
        {
            let core = source
                .roots()
                .any(|r| r.module.is_empty() && attribute(&r.node, "no_std").is_some());
            let krate = if core { "core" } else { "std" };
            return self.global(site, &format!("{krate}::{name}"));
        }
        Err(format!("definition of `{name}!` is unavailable").into())
    }

    pub fn expand(
        &mut self,
        source: &Source,
        parent: usize,
        at: &Location,
        call: &ast::MacroCall,
        mut imports: Option<&mut crate::flow::Imports>,
    ) -> Result<(usize, SyntaxNode)> {
        if self.definitions.is_empty() {
            self.load(source)?
        }
        let depth = if parent == 0 {
            1
        } else {
            self.views[parent - 1].depth + 1
        };
        let path = call.path().ok_or("macro call has no path")?;
        if depth > self.recursion_limit {
            return Err(format!(
                "`{path}!` exceeds the crate's macro recursion limit ({})",
                self.recursion_limit
            )
            .into());
        }
        let binding = self.resolve(source, parent, at, &path)?;
        if let Some(imports) = imports.as_deref_mut()
            && !resolve::path_name(&path).starts_with("$crate::")
        {
            imports.binding(&path.to_string(), &binding, |destination| {
                self.resolve(source, 0, destination, &path)
                    .is_ok_and(|b| binding.same_target(&b))
            })?;
        }
        let mut definition = binding.declaration;
        let input = call.token_tree().ok_or("macro call has no arguments")?;
        let origin = Origin {
            file: INPUT,
            edition: at.edition,
            parent: parent.checked_sub(1).map(|i| &self.views[i].expansion.map),
        };
        let site = origin.span_for(path.syntax().text_range());
        let generated = Origin {
            file: GENERATED,
            edition: site.ctx.edition(&self.db),
            parent: None,
        }
        .span_for(path.syntax().text_range());
        let args = syntax_bridge::syntax_node_to_token_tree(
            input.syntax(),
            &origin,
            generated,
            DocCommentDesugarMode::Mbe,
        );
        let mac =
            ast::Macro::cast(definition.node.clone()).ok_or("expected a declarative macro")?;
        let builtin = mac
            .attrs()
            .find(|a| {
                a.simple_name().as_deref() == Some("rustc_builtin_macro")
                    && !definition.krate.is_empty()
            })
            .map(|attr| {
                attr.as_simple_call()
                    .map(|(_, t)| t.to_string().trim_matches(['(', ')']).to_owned())
                    .unwrap_or_else(|| mac.name().unwrap().text().to_owned())
            });
        let rule = match builtin.as_deref() {
            Some("std_panic" | "core_panic" | "unreachable") => {
                let era = if site.ctx.edition(&self.db) >= Edition::Edition2021 {
                    "2021"
                } else {
                    "2015"
                };
                let name = mac.name().ok_or("builtin has no name")?;
                definition = self
                    .global(
                        &definition,
                        &format!("{}::panic::{}_{era}", definition.krate, name.text()),
                    )?
                    .declaration;
                self.declarative(&definition)?
            }
            Some(name) => Rule::Builtin(name.to_owned()),
            None => self.declarative(&definition)?,
        };
        let expansion = transcribe(&self.db, &rule, &args, call.syntax(), generated)
            .map_err(|e| format!("`{path}!` at {:?}: {e}", at.range()))?;
        let root = expansion.root.clone();
        if let Some(imports) = imports.filter(|i| i.has_local_uses)
            && matches!(rule, Rule::Builtin(_))
            && let Some(format) = root.descendants().find_map(ast::FormatArgsExpr::cast)
        {
            let input_range = format.template().and_then(|t| {
                let span = expansion.map.span_at(t.syntax().text_range().start());
                (span.anchor.file_id.file_id().index() == INPUT)
                    .then(|| self.source_range(parent, span.range))
                    .flatten()
            });
            builtin::captures(&format, |name| {
                if input_range.is_some_and(|r| imports.query(&self.definitions, r, name)) {
                    return Ok(());
                }
                imports.reference(
                    &self.definitions,
                    at,
                    self.parents(parent),
                    name,
                    Namespace::Value,
                )
            })?;
        }
        self.views.push(View {
            parent,
            depth,
            site: at.clone(),
            definition,
            input,
            rule,
            expansion,
        });
        Ok((self.views.len(), root))
    }

    fn exposes_declarations(
        &mut self,
        source: &Source,
        parent: usize,
        site: &Location,
        call: &ast::MacroCall,
    ) -> Result<bool> {
        let (view, root) = self.expand(source, parent, site, call, None)?;
        for n in root.children() {
            if let Some(call) = crate::symbol::statement_macro(n.clone()) {
                if self.exposes_declarations(
                    source,
                    view,
                    &site.at(call.syntax().clone()),
                    &call,
                )? {
                    return Ok(true);
                }
            } else if ast::LetStmt::can_cast(n.kind()) || ast::Item::can_cast(n.kind()) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn declarative(&self, definition: &Location) -> Result<Rule> {
        let origin = Origin {
            file: DEFINITION,
            edition: definition.edition,
            parent: None,
        };
        let tt = |tree: ast::TokenTree| {
            syntax_bridge::syntax_node_to_token_tree(
                tree.syntax(),
                &origin,
                origin.span_for(tree.syntax().text_range()),
                DocCommentDesugarMode::Mbe,
            )
        };
        let edition = |ctx: SyntaxContext| ctx.edition(&self.db);
        let mac =
            match ast::Macro::cast(definition.node.clone()).ok_or("expected a macro definition")? {
                ast::Macro::MacroRules(m) => DeclarativeMacro::parse_macro_rules(
                    &tt(m.token_tree().ok_or("missing macro rules")?),
                    edition,
                ),
                ast::Macro::MacroDef(m) => DeclarativeMacro::parse_macro2(
                    m.args().map(tt).as_ref(),
                    &tt(m.body().ok_or("missing macro body")?),
                    edition,
                ),
            };
        if let Some(e) = mac.err() {
            return Err(e.to_string().into());
        }
        Ok(Rule::Declarative(mac))
    }
}

fn tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|t| t.into_token())
}
