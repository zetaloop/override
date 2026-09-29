use std::collections::HashSet;

use ra_ap_syntax::{
    AstNode, Edition, SyntaxKind, SyntaxNode, ast,
    ast::{HasGenericArgs, HasName, HasTypeBounds},
};

use crate::{Result, Source, fragment, select::arguments, source::Location};

pub(crate) fn symbol(text: &str, edition: Edition) -> Result<String> {
    match fragment::ty(text, edition)? {
        ast::Type::PathType(path) => path
            .path()
            .map(|path| path_text(&path))
            .ok_or_else(|| "missing symbol path".into()),
        _ => Err(format!("expected a symbol path, got `{text}`").into()),
    }
}

pub(crate) fn matches_name(actual: &str, expected: &str) -> bool {
    actual == expected
        || actual
            .strip_suffix(expected)
            .is_some_and(|prefix| prefix.ends_with("::"))
}

pub(crate) fn qualified(location: &Location) -> String {
    let mut names = Vec::new();
    for node in location.ancestors() {
        if let Some(implementation) = ast::Impl::cast(node.clone()) {
            if let Some(ty) = implementation.self_ty() {
                names.push(fragment::spelling(ty.syntax()));
            }
        } else if (ast::Item::can_cast(node.kind())
            || matches!(node.kind(), SyntaxKind::VARIANT | SyntaxKind::RECORD_FIELD))
            && let Some(name) = node.children().find_map(ast::Name::cast)
        {
            names.push(name.text().to_string());
        }
    }
    names.reverse();
    names.join("::")
}

pub(crate) fn is_object(kind: SyntaxKind) -> bool {
    ast::Item::can_cast(kind)
        || ast::Expr::can_cast(kind)
        || matches!(
            kind,
            SyntaxKind::RECORD_FIELD
                | SyntaxKind::TUPLE_FIELD
                | SyntaxKind::RECORD_EXPR_FIELD
                | SyntaxKind::PARAM
                | SyntaxKind::SELF_PARAM
                | SyntaxKind::VARIANT
                | SyntaxKind::MATCH_ARM
        )
}

pub(crate) fn matches_path(path: &ast::Path, expected: &str) -> bool {
    matches_name(&path_text(path), expected) || matches_name(&path_name(path), expected)
}

pub(crate) fn path_text(path: &ast::Path) -> String {
    let parts = path
        .segments()
        .map(|segment| {
            let mut name = if let Some(name) = segment.name_ref() {
                name.text().to_string()
            } else if let Some(ast::PathSegmentKind::Type {
                type_ref,
                trait_ref,
            }) = segment.kind()
            {
                let ty = type_ref
                    .map(|ty| fragment::spelling(ty.syntax()))
                    .unwrap_or_default();
                let trait_ = trait_ref
                    .map(|ty| format!(" as {}", fragment::spelling(ty.syntax())))
                    .unwrap_or_default();
                format!("<{ty}{trait_}>")
            } else {
                String::new()
            };
            if let Some(arguments) = segment
                .syntax()
                .children()
                .find_map(ast::GenericArgList::cast)
            {
                let arguments = arguments
                    .generic_args()
                    .map(|argument| fragment::spelling(argument.syntax()))
                    .collect::<Vec<_>>()
                    .join(",");
                name.push_str(&format!("<{arguments}>"));
            }
            name
        })
        .collect::<Vec<_>>();
    let prefix = if path
        .syntax()
        .first_token()
        .is_some_and(|token| token.kind() == ra_ap_syntax::T![::])
    {
        "::"
    } else {
        ""
    };
    format!("{prefix}{}", parts.join("::"))
}

pub(crate) fn path_name(path: &ast::Path) -> String {
    let mut parts = path
        .qualifier()
        .map(|qualifier| path_name(&qualifier))
        .into_iter()
        .collect::<Vec<_>>();
    if let Some(segment) = path.segment() {
        if let Some(name) = segment.name_ref() {
            parts.push(name.text().to_string());
        } else if segment.type_anchor().is_some() {
            parts.push(path_text(&segment.parent_path()));
        }
    }
    let prefix = if path.qualifier().is_none()
        && path
            .syntax()
            .first_token()
            .is_some_and(|token| token.kind() == ra_ap_syntax::T![::])
    {
        "::"
    } else {
        ""
    };
    format!("{prefix}{}", parts.join("::"))
}

pub(crate) fn call_name(node: &SyntaxNode) -> Option<String> {
    if let Some(call) = ast::MethodCallExpr::cast(node.clone()) {
        return call.name_ref().map(|name| name.text().to_string());
    }
    match ast::CallExpr::cast(node.clone())?.expr()? {
        ast::Expr::PathExpr(path) => path.path().map(|path| path_name(&path)),
        _ => None,
    }
}

pub(crate) fn pattern_names(pattern: &ast::Pat) -> Vec<String> {
    pattern
        .syntax()
        .descendants()
        .filter_map(ast::IdentPat::cast)
        .filter_map(|binding| binding.name())
        .map(|name| name.text().to_string())
        .collect()
}

fn tuple_pattern_bindings(
    fields: impl IntoIterator<Item = ast::Pat>,
    field_count: usize,
) -> Result<Vec<(usize, ast::Pat)>> {
    let fields = fields.into_iter().collect::<Vec<_>>();
    let rest = fields
        .iter()
        .position(|field| ast::RestPat::can_cast(field.syntax().kind()));
    let explicit = fields.len() - usize::from(rest.is_some());
    if explicit > field_count {
        return Err("tuple pattern has more fields than its declaration".into());
    }
    let suffix = rest.map_or(0, |index| fields.len() - index - 1);
    Ok(fields
        .into_iter()
        .enumerate()
        .filter_map(|(position, field)| {
            if rest == Some(position) {
                return None;
            }
            let index = rest.map_or(position, |rest| {
                if position < rest {
                    position
                } else {
                    field_count - suffix + position - rest - 1
                }
            });
            Some((index, field))
        })
        .collect())
}

pub(crate) fn node_type(node: &SyntaxNode) -> Option<ast::Type> {
    ast::Type::cast(node.clone()).or_else(|| node.children().find_map(ast::Type::cast))
}

pub(crate) fn declarations(source: &Source) -> Vec<(String, Location)> {
    std::iter::once((source.module.as_str(), &source.root, source.edition))
        .chain(
            source
                .modules
                .iter()
                .map(|(name, root, edition)| (name.as_str(), root, *edition)),
        )
        .flat_map(|(module, root, edition)| {
            let mut location = Location::root(root.clone(), edition);
            location.module = module.to_owned();
            location
                .descendants()
                .into_iter()
                .map(move |location| (module.to_owned(), location))
        })
        .collect()
}

pub(crate) fn types_equal(
    source: &Source,
    context: &Location,
    left: &ast::Type,
    right: &ast::Type,
) -> bool {
    fn expand(source: &Source, mut context: Location, mut ty: ast::Type) -> ast::Type {
        let mut seen = HashSet::new();
        loop {
            if !matches!(ty, ast::Type::PathType(_)) {
                return ty;
            }
            let text = fragment::spelling(ty.syntax());
            if !seen.insert((module_path(&context), text.clone())) {
                return ty;
            }
            if context
                .ancestors()
                .iter()
                .flat_map(|node| node.children())
                .filter_map(ast::GenericParamList::cast)
                .flat_map(|list| list.generic_params())
                .any(|parameter| {
                    parameter
                        .syntax()
                        .children()
                        .find_map(ast::Name::cast)
                        .is_some_and(|name| name.text() == text)
                })
            {
                return ty;
            }
            let paths = paths(&context, &text);
            let aliases = declarations(source)
                .into_iter()
                .filter_map(|(module, location)| {
                    let alias = ast::TypeAlias::cast(location.node.clone())?;
                    let name = [module, qualified(&location)]
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join("::");
                    if !paths.contains(&name)
                        || alias
                            .syntax()
                            .children()
                            .any(|node| ast::GenericParamList::can_cast(node.kind()))
                    {
                        return None;
                    }
                    Some((location, alias.ty()?))
                })
                .collect::<Vec<_>>();
            match aliases.as_slice() {
                [(location, alias)] => {
                    context = location.clone();
                    ty = alias.clone();
                }
                _ => return ty,
            }
        }
    }
    let left = expand(source, context.clone(), left.clone());
    let right = expand(source, context.clone(), right.clone());
    fragment::equivalent(left.syntax(), right.syntax())
}

pub(crate) fn import_path(tree: &ast::UseTree) -> String {
    let mut parts = tree
        .syntax()
        .ancestors()
        .filter_map(ast::UseTree::cast)
        .filter_map(|tree| tree.path())
        .map(|path| path_name(&path))
        .collect::<Vec<_>>();
    parts.reverse();
    parts.join("::").trim_end_matches("::self").to_owned()
}

fn use_names(tree: &ast::UseTree, prefix: &str, output: &mut Vec<(String, String)>) {
    let path = tree.path().map(|path| path_name(&path)).unwrap_or_default();
    let path = [prefix, &path]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("::");
    if let Some(list) = tree.use_tree_list() {
        for child in list.use_trees() {
            use_names(&child, &path, output);
        }
    } else {
        let path = path.strip_suffix("::self").unwrap_or(&path).to_owned();
        let name = tree
            .rename()
            .and_then(|rename| rename.name())
            .map(|name| name.text().to_string())
            .unwrap_or_else(|| {
                if tree.star_token().is_some() {
                    "*".to_owned()
                } else {
                    path.rsplit("::").next().unwrap_or("").to_owned()
                }
            });
        output.push((name, path));
    }
}

fn imports(location: &Location) -> Vec<(String, String)> {
    let ancestors = location.ancestors();
    let mut output = Vec::new();
    for ancestor in &ancestors {
        for import in ancestor.children().filter_map(ast::Use::cast) {
            if let Some(tree) = import.use_tree() {
                use_names(&tree, "", &mut output);
            }
        }
    }
    output
}

pub(crate) fn module_path(location: &Location) -> String {
    let mut names = location
        .ancestors()
        .into_iter()
        .filter_map(ast::Module::cast)
        .filter_map(|module| module.name())
        .map(|name| name.text().to_string())
        .collect::<Vec<_>>();
    names.reverse();
    if !location.module.is_empty() {
        names.insert(0, location.module.clone());
    }
    names.join("::")
}

pub(crate) fn absolute(name: &str, module: &str) -> String {
    if let Some(name) = name
        .strip_prefix("crate::")
        .or_else(|| name.strip_prefix("::"))
    {
        return name.to_owned();
    }
    let mut parts = module
        .split("::")
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let mut name = name.strip_prefix("self::").unwrap_or(name);
    while let Some(tail) = name.strip_prefix("super::") {
        parts.pop();
        name = tail;
    }
    parts.push(name);
    parts.join("::")
}

fn paths(location: &Location, name: &str) -> Vec<String> {
    let mut current = name.to_owned();
    let aliases = imports(location);
    let mut seen = HashSet::new();
    while seen.insert(current.clone()) {
        let head = current.split("::").next().unwrap_or("");
        let Some((_, target)) = aliases.iter().find(|(alias, _)| alias == head) else {
            break;
        };
        current = format!("{target}{}", &current[head.len()..]);
    }
    let module = module_path(location);
    let mut paths = vec![absolute(&current, &module)];
    if !current.contains("::") {
        paths.extend(
            aliases
                .iter()
                .filter(|(alias, _)| alias == "*")
                .map(|(_, prefix)| absolute(&format!("{prefix}::{current}"), &module)),
        );
    }
    paths
}

fn expression_type(location: &Location, expression: &ast::Expr) -> Option<ast::Type> {
    match expression {
        ast::Expr::RefExpr(reference) => {
            let ty = expression_type(location, &reference.expr()?)?;
            fragment::ty(
                &format!(
                    "&{}{}",
                    if reference.mut_token().is_some() {
                        "mut "
                    } else {
                        ""
                    },
                    ty
                ),
                location.edition,
            )
            .ok()
        }
        ast::Expr::ParenExpr(paren) => expression_type(location, &paren.expr()?),
        ast::Expr::CastExpr(cast) => cast.ty(),
        ast::Expr::PathExpr(path) => {
            let name = path_name(&path.path()?);
            if name == "self" {
                return location
                    .ancestors()
                    .into_iter()
                    .find_map(ast::Impl::cast)?
                    .self_ty();
            }
            for ancestor in location.ancestors() {
                let parameters = ast::Fn::cast(ancestor.clone())
                    .and_then(|function| function.param_list())
                    .or_else(|| {
                        ast::ClosureExpr::cast(ancestor.clone())
                            .and_then(|closure| closure.param_list())
                    });
                if let Some(parameters) = parameters {
                    for parameter in parameters.params() {
                        if parameter
                            .pat()
                            .is_some_and(|pattern| pattern_names(&pattern).contains(&name))
                        {
                            return match parameter.pat()? {
                                ast::Pat::IdentPat(pattern)
                                    if pattern
                                        .name()
                                        .is_some_and(|binding| binding.text() == name) =>
                                {
                                    parameter.ty()
                                }
                                _ => None,
                            };
                        }
                    }
                }
                if let Some(list) = ast::StmtList::cast(ancestor) {
                    let binding = list
                        .statements()
                        .filter_map(|statement| match statement {
                            ast::Stmt::LetStmt(binding) => Some(binding),
                            _ => None,
                        })
                        .filter(|binding| {
                            binding.syntax().text_range().end()
                                <= expression.syntax().text_range().start()
                        })
                        .filter(|binding| {
                            binding
                                .pat()
                                .is_some_and(|pattern| pattern_names(&pattern).contains(&name))
                        })
                        .last();
                    if let Some(binding) = binding {
                        if !binding.pat().is_some_and(|pattern| matches!(pattern, ast::Pat::IdentPat(pattern) if pattern.name().is_some_and(|binding| binding.text() == name))) { return None; }
                        if let Some(ty) = binding.ty() {
                            return Some(ty);
                        }
                        if let Some(initializer) = binding.initializer() {
                            return expression_type(
                                &location.at(initializer.syntax().clone()),
                                &initializer,
                            );
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

pub(crate) fn expected_type(location: &Location) -> Option<ast::Type> {
    let mut node = location.node.clone();
    loop {
        let parent = node.parent()?;
        if let Some(binding) = ast::LetStmt::cast(parent.clone()) {
            return binding.ty();
        }
        if let Some(function) = ast::Fn::cast(parent.clone()) {
            return function.ret_type().and_then(|ret| ret.ty());
        }
        if let Some(closure) = ast::ClosureExpr::cast(parent.clone()) {
            return closure.ret_type().and_then(|ret| ret.ty());
        }
        if let Some(list) = ast::StmtList::cast(parent.clone()) {
            if list.tail_expr().as_ref().map(AstNode::syntax) != Some(&node) {
                return None;
            }
        } else if !matches!(
            parent.kind(),
            SyntaxKind::BLOCK_EXPR | SyntaxKind::PAREN_EXPR
        ) {
            return None;
        }
        node = parent;
    }
}

pub(crate) fn value_type(
    source: &Source,
    location: &Location,
    expression: &ast::Expr,
) -> Option<ast::Type> {
    expression_type(location, expression).or_else(|| {
        let declaration = call_declaration(
            source,
            &location.at(expression.syntax().clone()),
            ast::Fn::can_cast,
        )
        .ok()?;
        let ty = ast::Fn::cast(declaration.node.clone())?.ret_type()?.ty()?;
        fragment::ty(
            &type_text(source, &declaration, ty.syntax(), &[]),
            location.edition,
        )
        .ok()
    })
}

pub(crate) fn type_shape(
    source: &Source,
    location: &Location,
    ty: &ast::Type,
) -> Result<(String, Vec<String>)> {
    let mut context = location.clone();
    let mut ty = ty.clone();
    let mut seen = HashSet::new();
    loop {
        let ast::Type::PathType(path_type) = &ty else {
            return Err(format!("expected a return container, got `{ty}`").into());
        };
        let path = path_type.path().ok_or("type has no path")?;
        let name = path_name(&path);
        let arguments = path
            .segment()
            .and_then(|segment| segment.generic_arg_list())
            .map(|arguments| {
                arguments
                    .generic_args()
                    .map(|argument| type_text(source, &context, argument.syntax(), &[]))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !seen.insert((module_path(&context), ty.to_string())) {
            return Err("cyclic return type alias".into());
        }
        if context
            .ancestors()
            .iter()
            .flat_map(|node| node.children())
            .filter_map(ast::GenericParamList::cast)
            .flat_map(|list| list.generic_params())
            .any(|parameter| {
                parameter
                    .syntax()
                    .children()
                    .find_map(ast::Name::cast)
                    .is_some_and(|parameter| parameter.text() == name)
            })
        {
            return Err(format!("return container `{name}` is a generic parameter").into());
        }
        let candidates = nominal_types(source, &context, &ty.to_string());
        if let Some(declaration) = candidates.first() {
            let Some(alias) = ast::TypeAlias::cast(declaration.node.clone()) else {
                return Ok((qualified(declaration), arguments));
            };
            let parameters = alias
                .syntax()
                .children()
                .find_map(ast::GenericParamList::cast)
                .map(|list| list.generic_params().collect::<Vec<_>>())
                .unwrap_or_default();
            if arguments.len() > parameters.len() {
                return Err("type alias has too many arguments".into());
            }
            let mut substitutions = Vec::new();
            for (index, parameter) in parameters.iter().enumerate() {
                let name = parameter
                    .syntax()
                    .children()
                    .find(|node| matches!(node.kind(), SyntaxKind::NAME | SyntaxKind::LIFETIME))
                    .ok_or("type alias parameter has no name")?
                    .to_string();
                let argument = arguments
                    .get(index)
                    .cloned()
                    .or_else(|| match parameter {
                        ast::GenericParam::TypeParam(parameter) => parameter
                            .default_type()
                            .map(|ty| type_text(source, declaration, ty.syntax(), &substitutions)),
                        _ => None,
                    })
                    .ok_or_else(|| format!("type alias parameter `{name}` requires an argument"))?;
                substitutions.push((name, argument));
            }
            let body = alias.ty().ok_or("type alias has no body")?;
            let text = type_text(source, declaration, body.syntax(), &substitutions);
            ty = fragment::ty(&text, location.edition)?;
            context = declaration.clone();
            continue;
        }
        let mut resolved = name.clone();
        for (alias, target) in imports(&context) {
            if resolved == alias
                || resolved
                    .strip_prefix(&alias)
                    .is_some_and(|suffix| suffix.starts_with("::"))
            {
                resolved = format!("{target}{}", &resolved[alias.len()..]);
                break;
            }
        }
        let resolved = resolved.trim_start_matches("::");
        let canonical = match resolved {
            "Result" => "core::result::Result",
            "Option" => "core::option::Option",
            name => name,
        };
        return Ok((canonical.to_owned(), arguments));
    }
}

pub(crate) fn type_text(
    source: &Source,
    context: &Location,
    node: &SyntaxNode,
    substitutions: &[(String, String)],
) -> String {
    let imports = imports(context);
    let generics = context
        .ancestors()
        .into_iter()
        .flat_map(|node| node.children().collect::<Vec<_>>())
        .filter_map(ast::GenericParamList::cast)
        .flat_map(|list| list.generic_params().collect::<Vec<_>>())
        .filter_map(|parameter| parameter.syntax().children().find_map(ast::Name::cast))
        .map(|name| name.text().to_owned())
        .collect::<HashSet<_>>();
    let relatives = node
        .descendants()
        .filter_map(ast::Path::cast)
        .filter(|path| {
            path.segments().all(|segment| {
                matches!(
                    segment
                        .name_ref()
                        .map(|name| name.text().to_owned())
                        .as_deref(),
                    Some("self" | "super")
                )
            }) && !path
                .syntax()
                .parent()
                .and_then(ast::Path::cast)
                .is_some_and(|parent| {
                    parent
                        .segment()
                        .and_then(|segment| segment.name_ref())
                        .is_some_and(|name| matches!(name.text(), "self" | "super"))
                })
        })
        .map(|path| {
            let prefix = absolute(&format!("{}::", path_name(&path)), &module_path(context));
            (
                path.syntax().text_range(),
                format!("crate::{prefix}").trim_end_matches("::").to_owned(),
            )
        })
        .collect::<Vec<_>>();
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .map(|token| {
            if let Some((range, text)) = relatives
                .iter()
                .find(|(range, _)| range.contains_range(token.text_range()))
            {
                return if range.start() == token.text_range().start() {
                    text.clone()
                } else {
                    String::new()
                };
            }
            let name = token.text();
            let segment = token
                .parent()
                .and_then(|node| node.parent())
                .and_then(ast::PathSegment::cast);
            let head = segment
                .as_ref()
                .is_some_and(|segment| segment.parent_path().qualifier().is_none());
            let lifetime = token
                .parent()
                .is_some_and(|node| ast::Lifetime::can_cast(node.kind()));
            if (head || lifetime)
                && let Some((_, value)) = substitutions
                    .iter()
                    .find(|(parameter, _)| parameter == name)
            {
                return if head
                    && segment.is_some_and(|segment| {
                        segment
                            .parent_path()
                            .syntax()
                            .parent()
                            .is_some_and(|parent| ast::Path::can_cast(parent.kind()))
                    }) {
                    format!("<{value}>")
                } else {
                    value.clone()
                };
            }
            if !head || generics.contains(name) || name == "Self" {
                return name.to_owned();
            }
            if let Some((_, target)) = imports.iter().find(|(alias, _)| alias == name) {
                if target.starts_with("self::") || target.starts_with("super::") {
                    return format!("crate::{}", absolute(target, &module_path(context)));
                }
                return target.clone();
            }
            if let Some(declaration) = nominal_types(source, context, name).first() {
                if declaration
                    .node
                    .ancestors()
                    .any(|node| ast::StmtList::can_cast(node.kind()))
                {
                    return name.to_owned();
                }
                let full = [declaration.module.clone(), qualified(declaration)]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("::");
                return format!("crate::{full}");
            }
            name.to_owned()
        })
        .collect()
}

fn receiver_types(source: &Source, location: &Location, receiver: &ast::Expr) -> Vec<String> {
    let mut types = Vec::new();
    let mut generic_parameters = HashSet::new();
    if let Some(mut ty) = expression_type(location, receiver) {
        while let ast::Type::RefType(reference) = &ty {
            let Some(inner) = reference.ty() else { break };
            ty = inner;
        }
        types.push(fragment::spelling(ty.syntax()));
        let name = types[0].clone();
        for node in location.ancestors() {
            for parameter in node
                .children()
                .filter_map(ast::GenericParamList::cast)
                .flat_map(|list| list.generic_params())
            {
                if let ast::GenericParam::TypeParam(parameter) = &parameter
                    && let Some(name) = parameter.name()
                {
                    generic_parameters.insert(name.text().to_string());
                }
                if let ast::GenericParam::TypeParam(parameter) = parameter
                    && parameter
                        .name()
                        .is_some_and(|parameter| parameter.text() == name)
                    && let Some(bounds) = parameter.type_bound_list()
                {
                    types.extend(
                        bounds
                            .bounds()
                            .filter_map(|bound| bound.ty())
                            .map(|ty| fragment::spelling(ty.syntax())),
                    );
                }
            }
        }
    } else if let ast::Expr::RecordExpr(record) = receiver
        && let Some(path) = record.path()
    {
        types.push(fragment::spelling(path.syntax()));
    }
    types.retain(|name| !generic_parameters.contains(name));
    let mut seen = HashSet::new();
    let mut cursor = 0;
    while cursor < types.len() {
        let name = types[cursor].clone();
        cursor += 1;
        if !seen.insert(name.clone()) || generic_parameters.contains(&name) {
            continue;
        }
        let wanted = paths(location, &name);
        for (module, declaration) in declarations(source) {
            let full = [module, qualified(&declaration)]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("::");
            if let Some(alias) = ast::TypeAlias::cast(declaration.node.clone())
                && wanted.contains(&full)
                && let Some(mut ty) = alias.ty()
            {
                while let ast::Type::RefType(reference) = &ty {
                    let Some(inner) = reference.ty() else { break };
                    ty = inner;
                }
                if let ast::Type::PathType(ty) = ty
                    && let Some(path) = ty.path()
                {
                    types.extend(
                        paths(&declaration, &path_text(&path))
                            .into_iter()
                            .map(|name| format!("crate::{name}")),
                    );
                }
            }
        }
    }
    types
}

fn call_declaration(
    source: &Source,
    location: &Location,
    kind: impl Fn(SyntaxKind) -> bool,
) -> Result<Location> {
    if let Some(declaration) = &location.declaration {
        if !kind(declaration.node.kind()) {
            return Err("supplied declaration has the wrong kind".into());
        }
        return Ok(*declaration.clone());
    }
    let mut reference = location.clone();
    let mut name = call_name(&location.node).ok_or("selection requires a named call")?;
    let wanted = if let Some(call) = ast::MethodCallExpr::cast(location.node.clone()) {
        let receiver = call.receiver().ok_or("method call has no receiver")?;
        receiver_types(source, location, &receiver)
            .into_iter()
            .flat_map(|ty| paths(location, &format!("{ty}::{name}")))
            .collect::<Vec<_>>()
    } else {
        let mut visited = HashSet::new();
        'reference: loop {
            if !visited.insert((reference.node.clone(), name.clone())) {
                return Err("cyclic callable binding".into());
            }
            let head = name.split("::").next().ok_or("empty callable name")?;
            for ancestor in reference.ancestors() {
                if name == head {
                    if let Some(list) = ast::StmtList::cast(ancestor.clone()) {
                        let binding = list
                            .statements()
                            .filter_map(|statement| match statement {
                                ast::Stmt::LetStmt(binding) => Some(binding),
                                _ => None,
                            })
                            .filter(|binding| {
                                binding.syntax().text_range().end()
                                    <= reference.node.text_range().start()
                            })
                            .filter(|binding| {
                                binding.pat().is_some_and(|pattern| {
                                    pattern_names(&pattern)
                                        .iter()
                                        .any(|binding| binding == &name)
                                })
                            })
                            .last();
                        if let Some(binding) = binding {
                            if let Some(ast::Pat::IdentPat(pattern)) = binding.pat()
                                && pattern.name().is_some_and(|binding| binding.text() == name)
                                && let Some(ast::Expr::PathExpr(path)) = binding.initializer()
                                && let Some(path) = path.path()
                            {
                                name = path_name(&path);
                                reference = reference.at(path.syntax().clone());
                                continue 'reference;
                            }
                            return Err(format!(
                                "`{name}` is a local callable value; supply its declaration"
                            )
                            .into());
                        }
                    }
                    if ancestor
                        .children()
                        .filter_map(ast::ParamList::cast)
                        .flat_map(|parameters| parameters.params())
                        .any(|parameter| {
                            parameter.pat().is_some_and(|pattern| {
                                pattern_names(&pattern)
                                    .iter()
                                    .any(|binding| binding == &name)
                            })
                        })
                    {
                        return Err(format!(
                            "`{name}` is a callable parameter; supply its declaration"
                        )
                        .into());
                    }
                }
                if name != head
                    && ancestor
                        .children()
                        .filter_map(ast::GenericParamList::cast)
                        .flat_map(|parameters| parameters.generic_params())
                        .any(|parameter| {
                            parameter
                                .syntax()
                                .children()
                                .find_map(ast::Name::cast)
                                .is_some_and(|parameter| parameter.text() == head)
                        })
                {
                    return Err(format!(
                        "`{name}` depends on a generic type; supply its declaration"
                    )
                    .into());
                }
                if ancestor.kind() == SyntaxKind::STMT_LIST {
                    let local = ancestor.children().find(|node| {
                        let namespace = name == head
                            || matches!(
                                node.kind(),
                                SyntaxKind::MODULE
                                    | SyntaxKind::STRUCT
                                    | SyntaxKind::ENUM
                                    | SyntaxKind::UNION
                                    | SyntaxKind::TRAIT
                                    | SyntaxKind::TYPE_ALIAS
                            );
                        namespace
                            && ast::Item::can_cast(node.kind())
                            && node
                                .children()
                                .find_map(ast::Name::cast)
                                .is_some_and(|item| item.text() == head)
                    });
                    if let Some(local) = local {
                        let declaration = reference.at(local);
                        if name == head {
                            if kind(declaration.node.kind()) {
                                return Ok(declaration);
                            }
                            return Err(format!(
                                "`{name}` names a different kind of local declaration"
                            )
                            .into());
                        }
                        let owner = [declaration.module.clone(), qualified(&declaration)]
                            .into_iter()
                            .filter(|part| !part.is_empty())
                            .collect::<Vec<_>>()
                            .join("::");
                        break 'reference vec![format!("{owner}{}", &name[head.len()..])];
                    }
                }
            }
            break paths(&reference, &name);
        }
    };
    let candidates = declarations(source)
        .into_iter()
        .filter_map(|(module, declaration)| {
            if !kind(declaration.node.kind()) {
                return None;
            }
            let full = [module, qualified(&declaration)]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("::");
            wanted
                .iter()
                .any(|wanted| full == *wanted || full.strip_prefix("crate::") == Some(wanted))
                .then_some(declaration)
        })
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [declaration] => Ok(declaration.clone()),
        [] => Err(format!("declaration of `{name}` is unavailable; supply its declaration").into()),
        _ => Err(format!("declaration of `{name}` is ambiguous; specify its impl or trait").into()),
    }
}

pub(crate) fn argument(source: &Source, location: &Location, name: &str) -> Result<Location> {
    let declaration = call_declaration(source, location, ast::Fn::can_cast)?;
    let function =
        ast::Fn::cast(declaration.node).ok_or("argument declaration must be a function")?;
    let parameters = function
        .param_list()
        .ok_or("declaration has no parameters")?;
    let method = ast::MethodCallExpr::can_cast(location.node.kind());
    let receiver = parameters.self_param();
    if method && receiver.is_none() {
        return Err("a method call requires a declaration with a receiver".into());
    }
    let mut parameter_names = Vec::new();
    if receiver.is_some() && !method {
        parameter_names.push(vec!["self".to_owned()]);
    }
    parameter_names.extend(parameters.params().map(|parameter| {
        match parameter.pat() {
            Some(ast::Pat::IdentPat(pattern)) => pattern
                .name()
                .map(|name| vec![name.text().to_string()])
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }));
    let arguments = arguments(&location.node)
        .ok_or("call has no arguments")?
        .args()
        .collect::<Vec<_>>();
    if arguments.len() != parameter_names.len() {
        return Err("call and declaration have different argument counts".into());
    }
    if method && name == "self" {
        return Ok(location.at(ast::MethodCallExpr::cast(location.node.clone())
            .and_then(|call| call.receiver())
            .ok_or("missing receiver")?
            .syntax()
            .clone()));
    }
    let positions = parameter_names
        .iter()
        .enumerate()
        .filter(|(_, names)| names.iter().any(|parameter| parameter == name))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    match positions.as_slice() {
        [index] => Ok(location.at(arguments[*index].syntax().clone())),
        [] => Err(format!("declaration has no parameter named `{name}`").into()),
        _ => Err(format!("parameter `{name}` is ambiguous").into()),
    }
}

fn nominal_types(source: &Source, usage: &Location, text: &str) -> Vec<Location> {
    let mut usage = usage.clone();
    let mut text = text.to_owned();
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    loop {
        let Ok(mut ty) = fragment::ty(&text, usage.edition) else {
            break;
        };
        while let ast::Type::RefType(reference) = &ty {
            let Some(inner) = reference.ty() else { break };
            ty = inner;
        }
        let ast::Type::PathType(ty) = ty else { break };
        let Some(path) = ty.path() else { break };
        let name = path_name(&path);
        if !seen.insert((module_path(&usage), name.clone())) {
            break;
        }
        let Some(head) = name.split("::").next() else {
            break;
        };
        if usage
            .ancestors()
            .iter()
            .flat_map(|node| node.children())
            .filter_map(ast::GenericParamList::cast)
            .flat_map(|list| list.generic_params())
            .any(|parameter| {
                parameter
                    .syntax()
                    .children()
                    .find_map(ast::Name::cast)
                    .is_some_and(|parameter| parameter.text() == head)
            })
        {
            break;
        }
        let kind = |kind| {
            matches!(
                kind,
                SyntaxKind::STRUCT
                    | SyntaxKind::ENUM
                    | SyntaxKind::VARIANT
                    | SyntaxKind::UNION
                    | SyntaxKind::TYPE_ALIAS
            )
        };
        let local = usage
            .ancestors()
            .into_iter()
            .filter_map(ast::StmtList::cast)
            .find_map(|scope| {
                scope.syntax().children().find(|node| {
                    (kind(node.kind()) || node.kind() == SyntaxKind::MODULE)
                        && node
                            .children()
                            .find_map(ast::Name::cast)
                            .is_some_and(|name| name.text() == head)
                })
            });
        let candidates = if let Some(local) = local {
            let local = usage.at(local);
            if name == head {
                vec![local]
            } else {
                let wanted = format!("{}{}", qualified(&local), &name[head.len()..]);
                local
                    .descendants()
                    .into_iter()
                    .filter(|candidate| {
                        kind(candidate.node.kind()) && qualified(candidate) == wanted
                    })
                    .collect()
            }
        } else {
            let paths = paths(&usage, &name);
            declarations(source)
                .into_iter()
                .filter_map(|(module, candidate)| {
                    let full = [module, qualified(&candidate)]
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join("::");
                    (kind(candidate.node.kind()) && paths.contains(&full)).then_some(candidate)
                })
                .collect::<Vec<_>>()
        };
        let [candidate] = candidates.as_slice() else {
            break;
        };
        result.push(candidate.clone());
        let Some(alias) = ast::TypeAlias::cast(candidate.node.clone()) else {
            break;
        };
        let Some(ty) = alias.ty() else { break };
        text = fragment::spelling(ty.syntax());
        usage = candidate.clone();
    }
    result
}

fn references_type(source: &Source, usage: &Location, name: &str, target: &Location) -> bool {
    let references = nominal_types(source, usage, name);
    if let Some(record) =
        ast::RecordExpr::cast(target.node.clone()).and_then(|record| record.path())
    {
        let record = path_name(&record);
        let targets = nominal_types(source, target, &record);
        if references.is_empty() && targets.is_empty() {
            let target_paths = paths(target, &record);
            return paths(usage, name)
                .iter()
                .any(|path| target_paths.contains(path));
        }
        references
            .iter()
            .any(|reference| targets.iter().any(|target| reference.same(target)))
    } else {
        references.iter().any(|reference| reference.same(target))
    }
}

pub(crate) fn fields(source: &Source, location: &Location, name: &str) -> Result<Vec<Location>> {
    fragment::name(name, source.edition)?;
    if let Some(arguments) = arguments(&location.node) {
        let declaration = call_declaration(source, location, |kind| {
            matches!(kind, SyntaxKind::STRUCT | SyntaxKind::VARIANT)
        })?;
        let list = declaration
            .node
            .children()
            .find_map(ast::TupleFieldList::cast)
            .ok_or("constructor has no tuple fields")?;
        let members = list.fields().collect::<Vec<_>>();
        let arguments = arguments.args().collect::<Vec<_>>();
        if arguments.len() != members.len() {
            return Err("constructor and declaration have different field counts".into());
        }
        return fields(source, &declaration, name)?
            .into_iter()
            .map(|field| {
                let position = members
                    .iter()
                    .position(|member| member.syntax() == &field.node)
                    .ok_or("selected member does not belong to the constructor")?;
                let mut value = location.at(arguments[position].syntax().clone());
                value.argument = true;
                Ok(value)
            })
            .collect();
    }
    let list = location
        .node
        .children()
        .find(|node| {
            matches!(
                node.kind(),
                SyntaxKind::RECORD_FIELD_LIST
                    | SyntaxKind::RECORD_EXPR_FIELD_LIST
                    | SyntaxKind::TUPLE_FIELD_LIST
            )
        })
        .or_else(|| {
            ast::TypeAlias::cast(location.node.clone())
                .and_then(|alias| alias.ty())
                .filter(|ty| ast::TupleType::can_cast(ty.syntax().kind()))
                .map(|ty| ty.syntax().clone())
        })
        .ok_or("selected object has no fields")?;
    if matches!(
        list.kind(),
        SyntaxKind::RECORD_FIELD_LIST | SyntaxKind::RECORD_EXPR_FIELD_LIST
    ) {
        let mut names = vec![name.to_owned()];
        for (_, usage) in declarations(source) {
            if let Some(pattern) = ast::RecordPat::cast(usage.node.clone())
                && pattern.path().is_some_and(|path| {
                    references_type(source, &usage, &path_name(&path), location)
                })
                && let Some(fields) = pattern
                    .syntax()
                    .children()
                    .find_map(ast::RecordPatFieldList::cast)
            {
                for field in fields.fields() {
                    if field
                        .syntax()
                        .children()
                        .find_map(ast::Pat::cast)
                        .is_some_and(|pattern| {
                            pattern_names(&pattern)
                                .iter()
                                .any(|binding| binding == name)
                        })
                        && let Some(field) = field.name_ref()
                    {
                        names.push(field.text().to_string());
                    }
                }
            }
        }
        return Ok(list
            .children()
            .filter(|field| {
                field
                    .children()
                    .find_map(ast::Name::cast)
                    .is_some_and(|field| names.iter().any(|name| field.text() == name))
                    || ast::RecordExprField::cast(field.clone())
                        .and_then(|field| field.field_name())
                        .is_some_and(|field| names.iter().any(|name| field.text() == name))
            })
            .map(|field| location.at(field))
            .collect());
    }
    let field_count = list
        .children()
        .filter(|node| ast::TupleField::can_cast(node.kind()) || ast::Type::can_cast(node.kind()))
        .count();
    let mut positions = HashSet::new();
    for (_, usage) in declarations(source) {
        if let Some(pattern) = ast::TupleStructPat::cast(usage.node.clone())
            && pattern
                .path()
                .is_some_and(|path| references_type(source, &usage, &path_name(&path), location))
        {
            for (index, field) in tuple_pattern_bindings(pattern.fields(), field_count)? {
                if pattern_names(&field).iter().any(|binding| binding == name) {
                    positions.insert(index);
                }
            }
        }
        if let Some(binding) = ast::LetStmt::cast(usage.node.clone())
            && let Some(ast::Pat::TuplePat(pattern)) = binding.pat()
            && let Some(initializer) = binding.initializer()
            && let Some(mut ty) = binding
                .ty()
                .or_else(|| expression_type(&usage, &initializer))
        {
            while let ast::Type::RefType(reference) = &ty {
                let Some(inner) = reference.ty() else { break };
                ty = inner;
            }
            if references_type(source, &usage, &fragment::spelling(ty.syntax()), location) {
                for (index, field) in tuple_pattern_bindings(pattern.fields(), field_count)? {
                    if pattern_names(&field).iter().any(|binding| binding == name) {
                        positions.insert(index);
                    }
                }
            }
        }
        if let Some(function) = ast::Fn::cast(usage.node.clone())
            && function
                .name()
                .is_some_and(|function| function.text() == name)
            && usage
                .ancestors()
                .into_iter()
                .find_map(ast::Impl::cast)
                .and_then(|implementation| implementation.self_ty())
                .is_some_and(|ty| {
                    references_type(source, &usage, &fragment::spelling(ty.syntax()), location)
                })
        {
            for field in usage.node.descendants().filter_map(ast::FieldExpr::cast) {
                if field
                    .expr()
                    .is_some_and(|receiver| fragment::spelling(receiver.syntax()) == "self")
                {
                    for token in field
                        .syntax()
                        .children_with_tokens()
                        .flat_map(|element| match element {
                            ra_ap_syntax::SyntaxElement::Node(node)
                                if ast::NameRef::can_cast(node.kind()) =>
                            {
                                node.children_with_tokens().collect::<Vec<_>>()
                            }
                            element => vec![element],
                        })
                        .filter_map(|element| element.into_token())
                        .filter(|token| token.kind() == SyntaxKind::INT_NUMBER)
                    {
                        positions.insert(token.text().parse::<usize>()?);
                    }
                }
            }
        }
    }
    let fields = list
        .children()
        .filter(|node| ast::TupleField::can_cast(node.kind()) || ast::Type::can_cast(node.kind()))
        .collect::<Vec<_>>();
    let mut positions = positions.into_iter().collect::<Vec<_>>();
    positions.sort_unstable();
    positions
        .into_iter()
        .map(|index| {
            fields
                .get(index)
                .cloned()
                .map(|node| location.at(node))
                .ok_or_else(|| "binding refers to a missing tuple field".into())
        })
        .collect()
}
