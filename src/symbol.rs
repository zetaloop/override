use std::collections::HashSet;

use ra_ap_syntax::{
    AstNode, SyntaxKind, SyntaxNode, ast,
    ast::{HasGenericArgs, HasLoopBody, HasName},
};

use crate::{Result, Selected, Source, flow, fragment, resolve, source::Location};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Namespace {
    Type,
    Value,
    Macro,
}

#[derive(Clone)]
struct Binding {
    declaration: Location,
    unresolved: Option<String>,
    imports: Vec<(Location, Location, Option<String>)>,
}

impl From<Location> for Binding {
    fn from(declaration: Location) -> Self {
        Self {
            declaration,
            unresolved: None,
            imports: Vec::new(),
        }
    }
}

fn belongs(node: &SyntaxNode, namespace: Option<Namespace>) -> bool {
    let ty = matches!(
        node.kind(),
        SyntaxKind::STRUCT
            | SyntaxKind::ENUM
            | SyntaxKind::VARIANT
            | SyntaxKind::UNION
            | SyntaxKind::TRAIT
            | SyntaxKind::TYPE_ALIAS
            | SyntaxKind::MODULE
            | SyntaxKind::TYPE_PARAM
            | SyntaxKind::EXTERN_CRATE
    );
    let value = matches!(
        node.kind(),
        SyntaxKind::FN
            | SyntaxKind::CONST
            | SyntaxKind::STATIC
            | SyntaxKind::CONST_PARAM
            | SyntaxKind::IDENT_PAT
            | SyntaxKind::SELF_PARAM
    ) || (matches!(node.kind(), SyntaxKind::STRUCT | SyntaxKind::VARIANT)
        && !node
            .children()
            .any(|child| child.kind() == SyntaxKind::RECORD_FIELD_LIST));
    let mac = matches!(node.kind(), SyntaxKind::MACRO_RULES | SyntaxKind::MACRO_DEF);
    match namespace {
        Some(Namespace::Type) => ty,
        Some(Namespace::Value) => value,
        Some(Namespace::Macro) => mac,
        None => ty || value || mac,
    }
}

fn name(node: &SyntaxNode) -> Option<String> {
    node.children()
        .find_map(ast::Name::cast)
        .map(|name| name.text().to_owned())
}

fn full(location: &Location) -> String {
    [location.module.clone(), resolve::qualified(location)]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("::")
}

fn scopes(location: &Location) -> Vec<Location> {
    location
        .ancestors()
        .into_iter()
        .filter(|node| {
            matches!(
                node.kind(),
                SyntaxKind::SOURCE_FILE | SyntaxKind::ITEM_LIST | SyntaxKind::STMT_LIST
            ) && !(location.parents.last().is_some_and(|frame| {
                node == &location.root
                    || (frame.delimited && node.parent().as_ref() == Some(&location.root))
            }))
        })
        .map(|node| location.at(node))
        .collect()
}

fn context(location: &Location) -> Location {
    location
        .node
        .children()
        .find(|node| matches!(node.kind(), SyntaxKind::ITEM_LIST | SyntaxKind::STMT_LIST))
        .map_or_else(|| location.clone(), |node| location.at(node))
}

fn self_type(context: &Location) -> Option<(Location, ast::Path)> {
    let mut item = false;
    for node in context.ancestors() {
        let owner = context.at(node.clone());
        if let Some(implementation) = ast::Impl::cast(node.clone()) {
            let ast::Type::PathType(ty) = implementation.self_ty()? else {
                return None;
            };
            return Some((owner, ty.path()?));
        }
        if ast::Adt::can_cast(node.kind()) {
            let mut text = name(&node)?;
            if let Some(parameters) = node.children().find_map(ast::GenericParamList::cast) {
                let arguments = parameters
                    .generic_params()
                    .map(|parameter| {
                        parameter
                            .syntax()
                            .children()
                            .find(|node| {
                                matches!(node.kind(), SyntaxKind::NAME | SyntaxKind::LIFETIME)
                            })
                            .map(|node| node.to_string())
                    })
                    .collect::<Option<Vec<_>>>()?;
                text.push_str(&format!("<{}>", arguments.join(", ")));
            }
            let ast::Type::PathType(ty) = fragment::ty(&text, context.edition).ok()? else {
                return None;
            };
            return Some((owner, ty.path()?));
        }
        if node.kind() == SyntaxKind::TRAIT || item && node.kind() == SyntaxKind::STMT_LIST {
            return None;
        }
        if ast::Item::can_cast(node.kind()) && !ast::MacroCall::can_cast(node.kind()) {
            if item {
                return None;
            }
            item = true;
        }
    }
    None
}

fn same(left: &Location, right: &Location) -> bool {
    left.module == right.module && left.same(right)
}

fn unique(bindings: Vec<Binding>) -> Vec<Binding> {
    let mut output: Vec<Binding> = Vec::new();
    for binding in bindings {
        if !output
            .iter()
            .any(|other| match (&other.unresolved, &binding.unresolved) {
                (Some(left), Some(right)) => left == right,
                (None, None) => same(&other.declaration, &binding.declaration),
                _ => false,
            })
        {
            output.push(binding);
        }
    }
    output
}

fn locals(
    context: &Location,
    expected: &str,
    namespace: Option<Namespace>,
) -> Option<(Location, SyntaxNode)> {
    let mut capture = true;
    let mut generics = true;
    let mut crossed_item = false;
    for node in context.ancestors() {
        if crossed_item && node.kind() == SyntaxKind::STMT_LIST {
            generics = false;
        }
        if generics {
            for parameter in node
                .children()
                .filter_map(ast::GenericParamList::cast)
                .flat_map(|list| list.generic_params())
            {
                if belongs(parameter.syntax(), namespace)
                    && name(parameter.syntax()).as_deref() == Some(expected)
                {
                    return Some((context.at(parameter.syntax().clone()), node));
                }
            }
        }
        if capture && namespace != Some(Namespace::Type) && namespace != Some(Namespace::Macro) {
            let mut patterns = Vec::new();
            if let Some(list) = ast::StmtList::cast(node.clone()) {
                patterns.extend(list.statements().filter_map(|statement| match statement {
                    ast::Stmt::LetStmt(binding)
                        if context.at(binding.syntax().clone()).range().end()
                            <= context.range().start() =>
                    {
                        binding.pat()
                    }
                    _ => None,
                }));
                patterns.reverse();
            }
            if let Some(parameters) = node.children().find_map(ast::ParamList::cast) {
                let in_body = crate::select::body(&node)
                    .is_some_and(|body| context.at(body).range().contains_range(context.range()))
                    || context.node == node;
                if in_body {
                    patterns.extend(parameters.params().filter_map(|parameter| parameter.pat()));
                    if expected == "self"
                        && let Some(receiver) = parameters.self_param()
                    {
                        return Some((context.at(receiver.syntax().clone()), node));
                    }
                }
            }
            if let Some(expression) = ast::ForExpr::cast(node.clone())
                && expression.loop_body().is_some_and(|body| {
                    context
                        .at(body.syntax().clone())
                        .range()
                        .contains_range(context.range())
                })
            {
                patterns.extend(expression.pat());
            }
            if let Some(arm) = ast::MatchArm::cast(node.clone())
                && !arm.pat().is_some_and(|pattern| {
                    context
                        .at(pattern.syntax().clone())
                        .range()
                        .contains_range(context.range())
                })
            {
                patterns.extend(arm.pat());
            }
            let condition = if let Some(expression) = ast::IfExpr::cast(node.clone()) {
                expression
                    .condition()
                    .zip(expression.then_branch().map(|body| body.syntax().clone()))
            } else if let Some(expression) = ast::WhileExpr::cast(node.clone()) {
                expression
                    .condition()
                    .zip(expression.loop_body().map(|body| body.syntax().clone()))
            } else if let Some(arm) = ast::MatchArm::cast(node.clone()) {
                arm.guard()
                    .and_then(|guard| guard.condition())
                    .zip(arm.expr().map(|body| body.syntax().clone()))
            } else {
                None
            };
            if let Some((condition, body)) = condition
                && (context.at(body).range().contains_range(context.range())
                    || context
                        .at(condition.syntax().clone())
                        .range()
                        .contains_range(context.range()))
            {
                let mut bindings = condition
                    .syntax()
                    .descendants()
                    .filter_map(ast::LetExpr::cast)
                    .filter(|binding| {
                        context.at(binding.syntax().clone()).range().end()
                            <= context.range().start()
                    })
                    .filter(|binding| {
                        binding
                            .syntax()
                            .ancestors()
                            .skip(1)
                            .take_while(|parent| parent != condition.syntax())
                            .all(|parent| {
                                matches!(
                                    parent.kind(),
                                    SyntaxKind::BIN_EXPR | SyntaxKind::PAREN_EXPR
                                )
                            })
                    })
                    .filter_map(|binding| binding.pat())
                    .collect::<Vec<_>>();
                bindings.reverse();
                bindings.extend(patterns);
                patterns = bindings;
            }
            for pattern in patterns {
                if let Some(binding) = pattern
                    .syntax()
                    .descendants()
                    .filter_map(ast::IdentPat::cast)
                    .find(|binding| binding.name().is_some_and(|name| name.text() == expected))
                {
                    return Some((context.at(binding.syntax().clone()), node));
                }
            }
        }
        if ast::Item::can_cast(node.kind()) && !ast::MacroCall::can_cast(node.kind()) {
            capture = false;
            crossed_item = true;
        }
    }
    None
}

fn lookup(
    definitions: &[Location],
    context: &Location,
    text: &str,
    namespace: Option<Namespace>,
    visited: &mut HashSet<(SyntaxNode, String, Option<Namespace>)>,
) -> Vec<Binding> {
    if namespace.is_none() {
        let mut bindings = unique(
            [Namespace::Type, Namespace::Value, Namespace::Macro]
                .into_iter()
                .flat_map(|namespace| lookup(definitions, context, text, Some(namespace), visited))
                .collect(),
        );
        if bindings.iter().any(|binding| binding.unresolved.is_none()) {
            bindings.retain(|binding| {
                binding.unresolved.is_none()
                    || !binding.imports.is_empty()
                    || !same(&binding.declaration, context)
            });
        }
        return bindings;
    }
    let key = (context.node.clone(), text.to_owned(), namespace);
    if !visited.insert(key.clone()) {
        return Vec::new();
    }
    let result = lookup_inner(definitions, context, text, namespace, visited);
    visited.remove(&key);
    unique(result)
}

fn global(
    definitions: &[Location],
    context: &Location,
    text: &str,
    namespace: Option<Namespace>,
    visited: &mut HashSet<(SyntaxNode, String, Option<Namespace>)>,
) -> Vec<Binding> {
    let direct = definitions
        .iter()
        .filter(|location| {
            (ast::Item::can_cast(location.node.kind())
                || location.node.kind() == SyntaxKind::VARIANT)
                && full(location) == text
        })
        .cloned()
        .map(Binding::from)
        .collect::<Vec<_>>();
    if !direct.is_empty() {
        return direct
            .into_iter()
            .filter(|binding| belongs(&binding.declaration.node, namespace))
            .collect();
    }
    let (module, member) = text.rsplit_once("::").unwrap_or(("", text));
    let mut result = Vec::new();
    let mut found = false;
    for root in definitions.iter().filter(|location| {
        (location.node.kind() == SyntaxKind::SOURCE_FILE
            && location.parents.is_empty()
            && location.module == module)
            || (location.node.kind() == SyntaxKind::MODULE
                && full(location) == module
                && ast::Module::cast(location.node.clone())
                    .is_some_and(|module| module.item_list().is_some()))
    }) {
        found = true;
        let node = ast::Module::cast(root.node.clone())
            .and_then(|module| module.item_list())
            .map_or_else(|| root.node.clone(), |list| list.syntax().clone());
        result.extend(lookup(
            definitions,
            &root.at(node),
            member,
            namespace,
            visited,
        ));
    }
    if !found {
        result.push(Binding {
            declaration: context.clone(),
            unresolved: Some(text.to_owned()),
            imports: Vec::new(),
        });
    }
    result
}

fn lookup_inner(
    definitions: &[Location],
    context: &Location,
    text: &str,
    namespace: Option<Namespace>,
    visited: &mut HashSet<(SyntaxNode, String, Option<Namespace>)>,
) -> Vec<Binding> {
    if text.starts_with("crate::")
        || text.starts_with("::")
        || text.starts_with("self::")
        || text.starts_with("super::")
    {
        return global(
            definitions,
            context,
            &resolve::absolute(text, &resolve::module_path(context)),
            namespace,
            visited,
        );
    }
    if let Some((head, tail)) = text.split_once("::") {
        let mut result = Vec::new();
        for owner in lookup(definitions, context, head, Some(Namespace::Type), visited) {
            let wanted = format!(
                "{}::{tail}",
                owner
                    .unresolved
                    .clone()
                    .unwrap_or_else(|| full(&owner.declaration))
            );
            for mut binding in global(definitions, context, &wanted, namespace, visited) {
                binding.imports.extend(owner.imports.clone());
                result.push(binding);
            }
        }
        return result;
    }
    if text == "Self" {
        if namespace == Some(Namespace::Macro) {
            return Vec::new();
        }
        let Some((owner, path)) = self_type(context) else {
            return Vec::new();
        };
        if ast::Adt::can_cast(owner.node.kind()) {
            return if namespace == Some(Namespace::Type) {
                vec![owner.into()]
            } else {
                Vec::new()
            };
        }
        return lookup(
            definitions,
            &owner,
            &resolve::path_name(&path),
            namespace,
            visited,
        );
    }
    let local = locals(context, text, namespace);
    let ancestors = scopes(context);
    let mut declared = false;
    for node in context.ancestors() {
        if let Some((local, owner)) = &local
            && *owner == node
        {
            if let Some(pattern) = ast::IdentPat::cast(local.node.clone())
                && pattern.ref_token().is_none()
                && pattern.mut_token().is_none()
                && pattern.pat().is_none()
            {
                let bindings = lookup(definitions, local, text, namespace, visited);
                if bindings.iter().any(|binding| {
                    binding.unresolved.is_none()
                        && matches!(
                            binding.declaration.node.kind(),
                            SyntaxKind::CONST | SyntaxKind::STRUCT | SyntaxKind::VARIANT
                        )
                }) {
                    return bindings;
                }
            }
            return vec![local.clone().into()];
        }
        let Some(scope) = ancestors.iter().find(|scope| scope.node == node) else {
            continue;
        };
        let named = definitions
            .iter()
            .filter(|location| {
                ast::Item::can_cast(location.node.kind())
                    && !location
                        .node
                        .parent()
                        .is_some_and(|parent| parent.kind() == SyntaxKind::ASSOC_ITEM_LIST)
                    && name(&location.node).as_deref() == Some(text)
                    && scopes(location)
                        .into_iter()
                        .find(|parent| !parent.same(location))
                        .is_some_and(|parent| {
                            parent.same(scope)
                                || (parent.node.kind() != SyntaxKind::STMT_LIST
                                    && scope.node.kind() != SyntaxKind::STMT_LIST
                                    && resolve::module_path(&parent) == resolve::module_path(scope))
                        })
            })
            .collect::<Vec<_>>();
        declared |= !named.is_empty();
        let mut result = named
            .into_iter()
            .filter(|location| belongs(&location.node, namespace))
            .cloned()
            .map(Binding::from)
            .collect::<Vec<_>>();
        let mut glob = Vec::new();
        for import in scope.node.children().filter_map(ast::Use::cast) {
            let Some(tree) = import.use_tree() else {
                continue;
            };
            for leaf in tree
                .syntax()
                .descendants()
                .filter_map(ast::UseTree::cast)
                .filter(|tree| tree.use_tree_list().is_none())
            {
                let location = scope.at(leaf.syntax().clone());
                if location.same(context)
                    || leaf
                        .syntax()
                        .text_range()
                        .contains_range(context.node.text_range())
                        && location.root == context.root
                {
                    continue;
                }
                let target = resolve::import_path(&leaf);
                let alias = leaf
                    .rename()
                    .and_then(|rename| rename.name())
                    .map(|name| name.text().to_owned())
                    .unwrap_or_else(|| target.rsplit("::").next().unwrap_or("").to_owned());
                if leaf.star_token().is_some() {
                    let path = format!("{target}::{text}");
                    glob.extend(lookup(definitions, &location, &path, namespace, visited));
                } else if alias == text {
                    declared = true;
                    for mut binding in lookup(definitions, &location, &target, namespace, visited) {
                        binding.imports.push((
                            location.clone(),
                            binding.declaration.clone(),
                            binding.unresolved.clone(),
                        ));
                        result.push(binding);
                    }
                }
            }
        }
        if !result.is_empty() {
            return result;
        }
        if !glob.is_empty() {
            return glob;
        }
        if scope.node.kind() != SyntaxKind::STMT_LIST {
            break;
        }
    }
    if declared {
        Vec::new()
    } else {
        vec![Binding {
            declaration: context.clone(),
            unresolved: Some(resolve::absolute(text, &resolve::module_path(context))),
            imports: Vec::new(),
        }]
    }
}

fn path_namespace(path: &ast::Path) -> Option<Namespace> {
    let parent = path.syntax().parent()?;
    match parent.kind() {
        SyntaxKind::PATH
        | SyntaxKind::PATH_TYPE
        | SyntaxKind::RECORD_EXPR
        | SyntaxKind::RECORD_PAT
        | SyntaxKind::TUPLE_STRUCT_PAT => Some(Namespace::Type),
        SyntaxKind::PATH_EXPR | SyntaxKind::PATH_PAT => Some(Namespace::Value),
        SyntaxKind::MACRO_CALL => Some(Namespace::Macro),
        SyntaxKind::USE_TREE
            if ast::UseTree::cast(parent).is_some_and(|tree| {
                tree.use_tree_list().is_some() || tree.star_token().is_some()
            }) =>
        {
            Some(Namespace::Type)
        }
        _ => None,
    }
}

pub(crate) fn catch_all(source: &Source, scope: &Location, pattern: &ast::Pat) -> bool {
    match pattern {
        ast::Pat::WildcardPat(_) => true,
        ast::Pat::ParenPat(pattern) => pattern
            .pat()
            .is_some_and(|pattern| catch_all(source, scope, &pattern)),
        ast::Pat::OrPat(pattern) => pattern
            .pats()
            .any(|pattern| catch_all(source, scope, &pattern)),
        ast::Pat::IdentPat(pattern) => {
            if let Some(pattern) = pattern.pat() {
                return catch_all(source, scope, &pattern);
            }
            let Some(name) = pattern.name() else {
                return false;
            };
            pattern.ref_token().is_some()
                || pattern.mut_token().is_some()
                || lookup(
                    &resolve::declarations(source)
                        .into_iter()
                        .map(|(_, location)| location)
                        .collect::<Vec<_>>(),
                    scope,
                    name.text(),
                    Some(Namespace::Value),
                    &mut HashSet::new(),
                )
                .iter()
                .all(|binding| {
                    binding.unresolved.is_none()
                        && binding.declaration.node.kind() == SyntaxKind::IDENT_PAT
                })
        }
        _ => false,
    }
}

pub(crate) fn declaration(source: &Source, scope: &Location) -> Result<Location> {
    let name = scope.symbol.as_deref().ok_or("selection has no symbol")?;
    let definitions = resolve::declarations(source)
        .into_iter()
        .map(|(_, location)| location)
        .collect::<Vec<_>>();
    let candidates = lookup(
        &definitions,
        &context(scope),
        name,
        None,
        &mut HashSet::new(),
    );
    if let Some(declaration) = &scope.declaration {
        let matching = candidates
            .iter()
            .filter(|candidate| {
                candidate.unresolved.is_none()
                    && (same(&candidate.declaration, declaration)
                        || (full(&candidate.declaration) == full(declaration)
                            && fragment::equivalent(
                                &candidate.declaration.node,
                                &declaration.node,
                            )))
            })
            .collect::<Vec<_>>();
        if let [candidate] = matching.as_slice() {
            return Ok(candidate.declaration.clone());
        }
        if candidates.len() == 1 && candidates[0].unresolved.is_some() {
            return Ok(*declaration.clone());
        }
        return Err(format!("supplied declaration does not match symbol `{name}`").into());
    }
    match candidates.as_slice() {
        [binding] if binding.unresolved.is_none() => Ok(binding.declaration.clone()),
        [] | [_] => {
            Err(format!("declaration of `{name}` is unavailable; supply its declaration").into())
        }
        _ => Err(format!(
            "symbol `{name}` is ambiguous ({}); supply its declaration",
            candidates
                .iter()
                .map(|binding| format!(
                    "{} {:?}",
                    full(&binding.declaration),
                    binding.declaration.node.kind()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into()),
    }
}

fn references(source: &Source, scope: &Location) -> Result<Vec<Location>> {
    let target = scope
        .declaration
        .as_ref()
        .ok_or("symbol has no declaration")?;
    let definitions = resolve::declarations(source)
        .into_iter()
        .map(|(_, location)| location)
        .collect::<Vec<_>>();
    let original = lookup(
        &definitions,
        &context(scope),
        scope.symbol.as_deref().ok_or("selection has no symbol")?,
        None,
        &mut HashSet::new(),
    );
    let unresolved = match original.as_slice() {
        [binding] => binding.unresolved.as_deref(),
        _ => None,
    };
    let matches_target = |declaration: &Location, missing: Option<&str>, namespace| {
        if let Some(missing) = missing {
            unresolved == Some(missing) && belongs(&target.node, Some(namespace))
        } else {
            same(declaration, target)
        }
    };
    let mut result: Vec<Location> = Vec::new();
    for location in scope.descendants() {
        if let Some(tree) = ast::UseTree::cast(location.node.clone())
            && tree.use_tree_list().is_none()
            && tree.star_token().is_none()
        {
            let bindings = lookup(
                &definitions,
                &location,
                &resolve::import_path(&tree),
                None,
                &mut HashSet::new(),
            );
            if bindings.iter().any(|binding| {
                matches_target(
                    &binding.declaration,
                    binding.unresolved.as_deref(),
                    Namespace::Type,
                ) || matches_target(
                    &binding.declaration,
                    binding.unresolved.as_deref(),
                    Namespace::Value,
                ) || matches_target(
                    &binding.declaration,
                    binding.unresolved.as_deref(),
                    Namespace::Macro,
                )
            }) {
                if bindings.len() != 1 {
                    return Err(format!("import `{tree}` has ambiguous declarations").into());
                }
                if !result.iter().any(|previous| previous.same(&location)) {
                    result.push(location);
                }
            }
            continue;
        }
        let (path, namespace) = if let Some(path) = ast::Path::cast(location.node.clone()) {
            let Some(namespace) = path_namespace(&path) else {
                continue;
            };
            (resolve::path_name(&path), namespace)
        } else if let Some(pattern) = ast::IdentPat::cast(location.node.clone())
            && matches!(
                target.node.kind(),
                SyntaxKind::CONST | SyntaxKind::STRUCT | SyntaxKind::VARIANT
            )
            && pattern.ref_token().is_none()
            && pattern.mut_token().is_none()
            && pattern.pat().is_none()
        {
            let Some(name) = pattern.name() else { continue };
            (name.text().to_owned(), Namespace::Value)
        } else {
            continue;
        };
        let bindings = lookup(
            &definitions,
            &location,
            &path,
            Some(namespace),
            &mut HashSet::new(),
        );
        let matches = bindings
            .iter()
            .filter(|binding| {
                matches_target(
                    &binding.declaration,
                    binding.unresolved.as_deref(),
                    namespace,
                )
            })
            .collect::<Vec<_>>();
        if matches.is_empty() {
            continue;
        }
        if bindings.len() != 1 {
            return Err(format!("reference `{path}` has ambiguous declarations").into());
        }
        let binding = matches[0];
        let import = binding
            .imports
            .iter()
            .find(|(import, declaration, missing)| {
                scope.module == import.module
                    && scope
                        .parents
                        .first()
                        .map_or(&scope.root, |frame| &frame.root)
                        == import
                            .parents
                            .first()
                            .map_or(&import.root, |frame| &frame.root)
                    && scope.range().contains_range(import.range())
                    && matches_target(declaration, missing.as_deref(), namespace)
            });
        let shorthand = location
            .node
            .parent()
            .filter(|parent| ast::PathExpr::can_cast(parent.kind()))
            .and_then(|parent| parent.parent())
            .and_then(ast::RecordExprField::cast)
            .filter(|field| field.colon_token().is_none())
            .map(|field| location.at(field.syntax().clone()));
        let location = import.map_or_else(
            || shorthand.unwrap_or(location),
            |(import, _, _)| import.clone(),
        );
        if !result.iter().any(|previous| previous.same(&location)) {
            result.push(location);
        }
    }
    if result.is_empty() {
        return Err("selected scope has no resolved references to the symbol".into());
    }
    Ok(result)
}

pub(crate) fn redirect(selected: Selected<'_>, target: &str) -> Result<()> {
    if selected.position.is_some() || selected.flow != flow::Options::default() {
        return Err("symbol redirection has no insertion or extraction options".into());
    }
    let replacement = fragment::path(target, selected.source.edition)?;
    let mut references = references(selected.source, &selected.location)?;
    references.sort_by_key(|location| std::cmp::Reverse(location.range().start()));
    let mut source = selected.source.clone();
    for reference in references {
        let arguments = ast::Path::cast(reference.node.clone())
            .filter(|path| resolve::path_name(path) == "Self")
            .and_then(|_| self_type(&reference))
            .and_then(|(owner, path)| Some((owner, path.segment()?.generic_arg_list()?)))
            .filter(|_| {
                replacement
                    .segment()
                    .is_none_or(|segment| segment.generic_arg_list().is_none())
            })
            .map(|(owner, arguments)| {
                resolve::type_text(selected.source, &owner, arguments.syntax(), &[])
            });
        let rewritten =
            arguments.map(|arguments| format!("{target}::{}", arguments.trim_start_matches("::")));
        let target = rewritten.as_deref().unwrap_or(target);
        let mut selection = current(&mut source, &reference)?;
        if let Some(tree) = ast::UseTree::cast(selection.location.node.clone())
            && tree.rename().is_none()
        {
            let path = resolve::import_path(&tree);
            selection.rename(path.rsplit("::").next().ok_or("import has no name")?)?;
            selection = current(&mut source, &reference)?;
        }
        selection.redirect(target)?;
    }
    selected.source.root = source.root;
    Ok(())
}

fn current<'a>(source: &'a mut Source, reference: &Location) -> Result<Selected<'a>> {
    let mut root = Location::root(source.root.clone(), source.edition);
    root.module = source.module.clone();
    let location = root
        .descendants()
        .into_iter()
        .find(|location| {
            location.node.kind() == reference.node.kind()
                && location.range().start() == reference.range().start()
                && ast::Path::cast(location.node.clone()).map(|path| path.segments().count())
                    == ast::Path::cast(reference.node.clone()).map(|path| path.segments().count())
        })
        .ok_or("symbol reference is missing during redirection")?;
    Ok(Selected {
        source,
        location,
        flow: flow::Options::default(),
        position: None,
    })
}
