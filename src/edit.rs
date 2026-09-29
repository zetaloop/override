use std::path::Path;

use ra_ap_syntax::{
    AstNode, SyntaxElement, SyntaxKind, SyntaxNode, T,
    ast::{self, HasGenericArgs, HasName, HasVisibility, make},
    syntax_editor::{Position, Removable, SyntaxEditor},
};

use crate::{
    Boundary, Result, Selected, flow, fragment, region, region::Edge, resolve, select::arguments,
    source::Location,
};

impl Selected<'_> {
    pub fn at(mut self, boundary: Boundary) -> Self {
        self.position = Some(boundary);
        self
    }

    fn insertion(&mut self) -> Result<Option<(Location, Edge)>> {
        self.position
            .take()
            .map(|boundary| boundary.target(self.source, &self.location))
            .transpose()
    }

    pub fn rename(self, name: &str) -> Result<()> {
        self.edit(|editor, node, edition| {
            let new = fragment::name(name, edition)?;
            if let Some(tree) = ast::UseTree::cast(node.clone()) {
                if let Some(old) = tree.rename().and_then(|rename| rename.name()) {
                    editor.replace(old.syntax(), new.syntax());
                } else {
                    editor.insert_all(
                        Position::last_child_of(node),
                        vec![
                            whitespace(" "),
                            make::token(T![as]).into(),
                            whitespace(" "),
                            new.syntax().clone().into(),
                        ],
                    );
                }
            } else {
                let old = node
                    .children()
                    .find_map(ast::Name::cast)
                    .ok_or("selected object has no declaration name")?;
                editor.replace(old.syntax(), new.syntax());
            }
            Ok(())
        })
    }

    pub fn set_visibility(self, visibility: &str) -> Result<()> {
        self.edit(|editor, node, edition| {
            if !matches!(
                node.kind(),
                SyntaxKind::FN
                    | SyntaxKind::STRUCT
                    | SyntaxKind::ENUM
                    | SyntaxKind::UNION
                    | SyntaxKind::TRAIT
                    | SyntaxKind::MODULE
                    | SyntaxKind::TYPE_ALIAS
                    | SyntaxKind::CONST
                    | SyntaxKind::STATIC
                    | SyntaxKind::RECORD_FIELD
                    | SyntaxKind::TUPLE_FIELD
                    | SyntaxKind::USE
                    | SyntaxKind::EXTERN_CRATE
            ) {
                return Err("selected object has no visibility".into());
            }
            let function = fragment::signature(&format!("{visibility} fn f()"), edition)?;
            let old = node.children().find_map(ast::Visibility::cast);
            match (old, function.visibility()) {
                (Some(old), Some(new)) => editor.replace(old.syntax(), new.syntax()),
                (Some(old), None) => editor.delete(old.syntax()),
                (None, Some(new)) => editor.insert_all(
                    Position::before(header(node)?),
                    vec![new.syntax().clone().into(), whitespace(" ")],
                ),
                (None, None) => {}
            }
            Ok(())
        })
    }

    pub fn add_attribute(mut self, attribute: &str) -> Result<()> {
        let position = self.insertion()?;
        self.edit(|editor, node, edition| {
            let root = fragment::file(&format!("{attribute}\nstruct S;"), edition)?;
            let attribute = fragment::one::<ast::AnyAttr>(&root)?;
            let position = if let Some(position) = &position {
                let index = insertion(node, position)?;
                let contents = node.children_with_tokens().collect::<Vec<_>>();
                if contents[..index].iter().any(|element| {
                    !element.kind().is_trivia() && !ast::AnyAttr::can_cast(element.kind())
                }) {
                    return Err("attribute insertion must precede the declaration header".into());
                }
                contents.get(index).map_or_else(
                    || Position::last_child_of(node),
                    |element| Position::before(element.clone()),
                )
            } else {
                Position::first_child_of(node)
            };
            editor.insert_all(
                position,
                vec![attribute.syntax().clone().into(), whitespace("\n")],
            );
            Ok(())
        })
    }

    pub fn add_field(mut self, declaration: &str) -> Result<()> {
        let position = self.insertion()?;
        self.edit(|editor, node, edition| {
            let list = node
                .children()
                .find(|node| {
                    matches!(
                        node.kind(),
                        SyntaxKind::RECORD_FIELD_LIST
                            | SyntaxKind::TUPLE_FIELD_LIST
                            | SyntaxKind::RECORD_EXPR_FIELD_LIST
                    )
                })
                .ok_or("selected object has no field list")?;
            let field = match list.kind() {
                SyntaxKind::RECORD_FIELD_LIST => {
                    let parsed = fragment::file(&format!("struct S {{ {declaration} }}"), edition)?;
                    fragment::one::<ast::RecordField>(&parsed)?.syntax().clone()
                }
                SyntaxKind::TUPLE_FIELD_LIST => {
                    let parsed = fragment::file(&format!("struct S({declaration});"), edition)?;
                    fragment::one::<ast::TupleField>(&parsed)?.syntax().clone()
                }
                SyntaxKind::RECORD_EXPR_FIELD_LIST => {
                    let parsed = fragment::expression(&format!("S {{ {declaration} }}"), edition)?;
                    fragment::one::<ast::RecordExprField>(parsed.syntax())?
                        .syntax()
                        .clone()
                }
                _ => unreachable!(),
            };
            append(editor, &list, field.into(), position.as_ref())
        })
    }

    pub fn add_variant(mut self, declaration: &str) -> Result<()> {
        let position = self.insertion()?;
        self.edit(|editor, node, edition| {
            let list = ast::Enum::cast(node.clone())
                .and_then(|node| node.variant_list())
                .ok_or("selected object is not an enum")?;
            let root = fragment::file(&format!("enum E {{ {declaration} }}"), edition)?;
            append(
                editor,
                list.syntax(),
                fragment::one::<ast::Variant>(&root)?
                    .syntax()
                    .clone()
                    .into(),
                position.as_ref(),
            )
        })
    }

    pub fn add_arm(mut self, declaration: &str) -> Result<()> {
        let position = self.insertion()?;
        self.edit(|editor, node, edition| {
            let list = ast::MatchExpr::cast(node.clone())
                .and_then(|expression| expression.match_arm_list())
                .ok_or("selected object is not a match")?;
            let parsed = fragment::expression(&format!("match () {{ {declaration} }}"), edition)?;
            append(
                editor,
                list.syntax(),
                fragment::one::<ast::MatchArm>(parsed.syntax())?
                    .syntax()
                    .clone()
                    .into(),
                position.as_ref(),
            )
        })
    }

    pub fn add_parameter(mut self, declaration: &str) -> Result<()> {
        let position = self.insertion()?;
        self.edit(|editor, node, edition| {
            let list = node
                .children()
                .find_map(ast::ParamList::cast)
                .ok_or("selected object has no parameter list")?;
            let function = fragment::signature(&format!("fn f({declaration})"), edition)?;
            let parameters = function.param_list().ok_or("missing parameter list")?;
            let parameters = parameters
                .syntax()
                .children()
                .filter(|node| matches!(node.kind(), SyntaxKind::PARAM | SyntaxKind::SELF_PARAM))
                .collect::<Vec<_>>();
            let [parameter] = parameters.as_slice() else {
                return Err("expected one parameter".into());
            };
            if ast::SelfParam::can_cast(parameter.kind()) && position.is_none() {
                if list.self_param().is_some() {
                    return Err("function already has a receiver".into());
                }
                let open = list.l_paren_token().ok_or("missing opening parenthesis")?;
                let mut elements = vec![parameter.clone().into()];
                if list.params().next().is_some() {
                    elements.extend([make::token(T![,]).into(), whitespace(" ")]);
                }
                editor.insert_all(Position::after(open), elements);
                Ok(())
            } else {
                append(
                    editor,
                    list.syntax(),
                    parameter.clone().into(),
                    position.as_ref(),
                )
            }
        })
    }

    pub fn add_generic(mut self, declaration: &str) -> Result<()> {
        let position = self.insertion()?;
        self.edit(|editor, node, edition| {
            let function = fragment::signature(&format!("fn f<{declaration}>()"), edition)?;
            let list = function
                .syntax()
                .children()
                .find_map(ast::GenericParamList::cast)
                .ok_or("missing generic parameters")?;
            let parameters = list.generic_params().collect::<Vec<_>>();
            let [parameter] = parameters.as_slice() else {
                return Err("expected one generic parameter".into());
            };
            if let Some(existing) = node.children().find_map(ast::GenericParamList::cast) {
                append(
                    editor,
                    existing.syntax(),
                    parameter.syntax().clone().into(),
                    position.as_ref(),
                )
            } else {
                let name = node
                    .children()
                    .find_map(ast::Name::cast)
                    .ok_or("selected object has no name")?;
                if let Some(position) = &position {
                    insertion(node, position)?;
                }
                editor.insert(Position::after(name.syntax()), list.syntax());
                Ok(())
            }
        })
    }

    pub fn set_bounds(self, bounds: &str) -> Result<()> {
        self.edit(|editor, node, edition| {
            let signature = fragment::signature(&format!("fn f<T: {bounds}>()"), edition)?;
            let list = fragment::one::<ast::TypeBoundList>(signature.syntax())?;
            if let Some(old) = node.children().find_map(ast::TypeBoundList::cast) {
                editor.replace(old.syntax(), list.syntax());
            } else {
                if !matches!(
                    node.kind(),
                    SyntaxKind::TYPE_PARAM
                        | SyntaxKind::LIFETIME_PARAM
                        | SyntaxKind::TRAIT
                        | SyntaxKind::TYPE_ALIAS
                ) {
                    return Err("selected object has no bounds".into());
                }
                let anchor = node
                    .children()
                    .filter(|node| {
                        matches!(
                            node.kind(),
                            SyntaxKind::NAME
                                | SyntaxKind::LIFETIME
                                | SyntaxKind::GENERIC_PARAM_LIST
                        )
                    })
                    .last()
                    .ok_or("missing bound owner")?;
                editor.insert_all(
                    Position::after(anchor),
                    vec![
                        make::token(T![:]).into(),
                        whitespace(" "),
                        list.syntax().clone().into(),
                    ],
                );
            }
            Ok(())
        })
    }

    pub fn set_where_clause(self, clause: &str) -> Result<()> {
        self.edit(|editor, node, edition| {
            if !matches!(
                node.kind(),
                SyntaxKind::FN
                    | SyntaxKind::STRUCT
                    | SyntaxKind::ENUM
                    | SyntaxKind::UNION
                    | SyntaxKind::TRAIT
                    | SyntaxKind::IMPL
                    | SyntaxKind::TYPE_ALIAS
            ) {
                return Err("selected object has no where clause".into());
            }
            let signature = fragment::signature(&format!("fn f() {clause}"), edition)?;
            let new = signature
                .syntax()
                .children()
                .find_map(ast::WhereClause::cast);
            let old = node.children().find_map(ast::WhereClause::cast);
            match (old, new) {
                (Some(old), Some(new)) => editor.replace(old.syntax(), new.syntax()),
                (Some(old), None) => editor.delete(old.syntax()),
                (None, Some(new)) => {
                    let anchor = node
                        .children_with_tokens()
                        .find(|element| {
                            matches!(
                                element.kind(),
                                SyntaxKind::BLOCK_EXPR
                                    | SyntaxKind::RECORD_FIELD_LIST
                                    | SyntaxKind::VARIANT_LIST
                                    | SyntaxKind::ASSOC_ITEM_LIST
                            ) || element.kind() == T![;]
                                || element.kind() == T![=]
                        })
                        .ok_or("missing declaration body")?;
                    editor.insert_all(
                        Position::before(anchor),
                        vec![
                            whitespace(" "),
                            new.syntax().clone().into(),
                            whitespace(" "),
                        ],
                    );
                }
                (None, None) => {}
            }
            Ok(())
        })
    }

    pub fn set_type(self, ty: &str) -> Result<()> {
        self.edit(|editor, node, edition| {
            let new = fragment::ty(ty, edition)?;
            if ast::Type::can_cast(node.kind()) {
                editor.replace(node, new.syntax());
            } else {
                let old = resolve::node_type(node).ok_or("selected object has no declared type")?;
                editor.replace(old.syntax(), new.syntax());
            }
            Ok(())
        })
    }

    pub fn set_return_type(self, ty: &str) -> Result<()> {
        self.edit(|editor, node, edition| {
            let function =
                ast::Fn::cast(node.clone()).ok_or("selected object is not a function")?;
            let signature = fragment::signature(&format!("fn f() -> {ty}"), edition)?;
            let new = signature.ret_type().ok_or("missing return type")?;
            if let Some(old) = function.ret_type() {
                editor.replace(old.syntax(), new.syntax());
            } else {
                let params = function.param_list().ok_or("missing parameters")?;
                editor.insert_all(
                    Position::after(params.syntax()),
                    vec![whitespace(" "), new.syntax().clone().into()],
                );
            }
            Ok(())
        })
    }

    pub fn set_signature(self, signature: &str) -> Result<()> {
        self.edit(|editor, node, edition| {
            let old = ast::Fn::cast(node.clone()).ok_or("selected object is not a function")?;
            let new = fragment::signature(signature, edition)?;
            let (replacement, function) = SyntaxEditor::with_ast_node(&new);
            let body = function.body().ok_or("signature has no body")?;
            if let Some(original) = old.body() {
                replacement.replace(body.syntax(), original.syntax());
            } else {
                replacement.replace(body.syntax(), make::token(T![;]));
            }
            let attributes = node
                .children_with_tokens()
                .take_while(|element| match element {
                    SyntaxElement::Node(node) => ast::AnyAttr::can_cast(node.kind()),
                    SyntaxElement::Token(token) => token.kind().is_trivia(),
                })
                .collect::<Vec<_>>();
            replacement.insert_all(Position::first_child_of(function.syntax()), attributes);
            editor.replace(node, replacement.finish().new_root());
            Ok(())
        })
    }

    pub fn set_value(self, value: &str) -> Result<()> {
        let argument = self.location.argument;
        self.edit(|editor, node, edition| {
            let expression = fragment::expression(value, edition)?;
            if argument {
                editor.replace(node, expression.syntax());
                return Ok(());
            }
            if let Some(field) = ast::RecordExprField::cast(node.clone()) {
                if let Some(old) = field.expr() {
                    editor.replace(old.syntax(), expression.syntax());
                } else {
                    let name = field.name_ref().ok_or("field has no name")?;
                    editor.insert_all(
                        Position::after(name.syntax()),
                        vec![
                            make::token(T![:]).into(),
                            whitespace(" "),
                            expression.syntax().clone().into(),
                        ],
                    );
                }
                return Ok(());
            }
            let value = ast::Const::cast(node.clone())
                .and_then(|node| node.body())
                .or_else(|| ast::Static::cast(node.clone()).and_then(|node| node.body()))
                .or_else(|| {
                    ast::Variant::cast(node.clone())
                        .and_then(|node| node.const_arg())
                        .and_then(|argument| argument.syntax().children().find_map(ast::Expr::cast))
                });
            if let Some(old) = value {
                editor.replace(old.syntax(), expression.syntax());
                return Ok(());
            }
            if matches!(
                node.kind(),
                SyntaxKind::CONST | SyntaxKind::STATIC | SyntaxKind::VARIANT
            ) {
                let elements = vec![
                    whitespace(" "),
                    make::token(T![=]).into(),
                    whitespace(" "),
                    expression.syntax().clone().into(),
                ];
                if let Some(semicolon) = node.last_token().filter(|token| token.kind() == T![;]) {
                    editor.insert_all(Position::before(semicolon), elements);
                } else {
                    editor.insert_all(Position::last_child_of(node), elements);
                }
                return Ok(());
            }
            Err("selected object has no named value slot".into())
        })
    }

    pub fn add_argument(mut self, value: &str) -> Result<()> {
        let position = self.insertion()?;
        self.edit(|editor, node, edition| {
            let list = arguments(node).ok_or("selected object is not a call")?;
            append(
                editor,
                list.syntax(),
                fragment::expression(value, edition)?
                    .syntax()
                    .clone()
                    .into(),
                position.as_ref(),
            )
        })
    }

    pub fn set_condition(self, condition: &str) -> Result<()> {
        self.edit(|editor, node, edition| {
            let old = ast::IfExpr::cast(node.clone())
                .and_then(|node| node.condition())
                .or_else(|| ast::WhileExpr::cast(node.clone()).and_then(|node| node.condition()))
                .or_else(|| ast::ForExpr::cast(node.clone()).and_then(|node| node.iterable()))
                .ok_or("selected object has no condition")?;
            editor.replace(
                old.syntax(),
                fragment::expression(condition, edition)?.syntax(),
            );
            Ok(())
        })
    }

    pub fn remove(self) -> Result<()> {
        let argument = self.location.argument;
        self.edit(|editor, node, _| {
            if let Some(tree) = ast::UseTree::cast(node.clone()) {
                let branch = import_branch(&tree)?;
                if let Some(tree) = ast::UseTree::cast(branch.clone()) {
                    tree.remove(editor);
                } else {
                    editor.delete(branch);
                }
                return Ok(());
            }
            let item = ast::Item::can_cast(node.kind())
                && (!ast::MacroCall::can_cast(node.kind())
                    || node.parent().is_some_and(|parent| {
                        matches!(
                            parent.kind(),
                            SyntaxKind::SOURCE_FILE
                                | SyntaxKind::ITEM_LIST
                                | SyntaxKind::ASSOC_ITEM_LIST
                        )
                    }));
            if !(argument
                || item
                || ast::AnyAttr::can_cast(node.kind())
                || matches!(
                    node.kind(),
                    SyntaxKind::RECORD_FIELD
                        | SyntaxKind::TUPLE_FIELD
                        | SyntaxKind::RECORD_EXPR_FIELD
                        | SyntaxKind::VARIANT
                        | SyntaxKind::PARAM
                        | SyntaxKind::SELF_PARAM
                        | SyntaxKind::TYPE_PARAM
                        | SyntaxKind::CONST_PARAM
                        | SyntaxKind::LIFETIME_PARAM
                ))
            {
                return Err("selected object is not a removable declaration or member".into());
            }
            if let Some(next) = std::iter::successors(node.next_sibling_or_token(), |element| {
                element.next_sibling_or_token()
            })
            .find(|element| !element.kind().is_trivia())
                && next.kind() == T![,]
            {
                editor.delete(next);
            }
            editor.delete(node);
            Ok(())
        })
    }

    pub fn add_use(mut self, declaration: &str) -> Result<()> {
        let position = self.insertion()?;
        self.edit(|editor, node, edition| {
            let parsed = fragment::file(declaration, edition)?;
            let import = fragment::one::<ast::Use>(&parsed)?;
            if parsed.children().count() != 1 {
                return Err("expected one use declaration".into());
            }
            insert_item(editor, node, import.syntax(), position.as_ref())
        })
    }

    pub fn mount_module(mut self, declaration: &str, path: impl AsRef<Path>) -> Result<()> {
        let position = self.insertion()?;
        self.edit(|editor, node, edition| {
            let path = path.as_ref().to_str().ok_or("module path is not UTF-8")?;
            let parsed = fragment::file(&format!("#[path = {path:?}]\n{declaration};"), edition)?;
            let module = fragment::one::<ast::Module>(&parsed)?;
            if parsed.children().count() != 1 {
                return Err("expected one module declaration".into());
            }
            insert_item(editor, node, module.syntax(), position.as_ref())
        })
    }

    pub fn redirect(self, target: &str) -> Result<()> {
        self.edit(|editor, node, edition| {
            let path = fragment::path(target, edition)?;
            if let Some(call) = ast::MethodCallExpr::cast(node.clone()) {
                if path.qualifier().is_some() {
                    return Err("use delegate to redirect a method to a qualified function".into());
                }
                let name = call.name_ref().ok_or("method call has no name")?;
                editor.replace(
                    name.syntax(),
                    path.segment()
                        .and_then(|segment| segment.name_ref())
                        .ok_or("target has no name")?
                        .syntax(),
                );
            } else if let Some(call) = ast::CallExpr::cast(node.clone()) {
                let original = call.expr().ok_or("call has no target")?;
                let generics = match &original {
                    ast::Expr::PathExpr(path) => path
                        .path()
                        .and_then(|path| path.segment())
                        .and_then(|segment| segment.generic_arg_list()),
                    _ => None,
                };
                let replacement = if path
                    .segment()
                    .and_then(|segment| segment.generic_arg_list())
                    .is_none()
                    && let Some(generics) = generics
                {
                    fragment::expression(
                        &format!("{}{}", path.syntax(), generics.syntax()),
                        edition,
                    )?
                } else {
                    make::expr_path(path)
                };
                editor.replace(original.syntax(), replacement.syntax());
            } else if let Some(call) = ast::MacroCall::cast(node.clone()) {
                editor.replace(
                    call.path().ok_or("macro has no path")?.syntax(),
                    path.syntax(),
                );
            } else if let Some(tree) = ast::UseTree::cast(node.clone()) {
                let original = node
                    .ancestors()
                    .find_map(ast::Use::cast)
                    .ok_or("import has no use declaration")?;
                let suffix = if let Some(list) = tree.use_tree_list() {
                    format!("::{}", list.syntax())
                } else if tree.star_token().is_some() {
                    "::*".to_owned()
                } else {
                    String::new()
                };
                let alias = tree
                    .rename()
                    .map(|rename| format!(" {}", rename.syntax()))
                    .unwrap_or_default();
                let root =
                    fragment::file(&format!("use {}{suffix}{alias};", path.syntax()), edition)?;
                let new = fragment::one::<ast::Use>(&root)?
                    .use_tree()
                    .ok_or("replacement import has no tree")?;
                let (replacement, import) = SyntaxEditor::with_ast_node(&original);
                replacement.replace(
                    import.use_tree().ok_or("import has no tree")?.syntax(),
                    new.syntax(),
                );
                let replacement = replacement.finish().new_root().clone();
                let branch = import_branch(&tree)?;
                if let Some(branch) = ast::UseTree::cast(branch.clone()) {
                    branch.remove(editor);
                    let indentation = ast::edit::IndentLevel::from_node(original.syntax());
                    editor.insert_all(
                        Position::after(original.syntax()),
                        vec![whitespace(&format!("\n{indentation}")), replacement.into()],
                    );
                } else {
                    editor.replace(branch, replacement);
                }
            } else {
                return Err("selected object is not a call, macro or import".into());
            }
            Ok(())
        })
    }

    pub fn delegate(mut self, helper: &str, context: &[&str]) -> Result<()> {
        let region = self.location.region.take();
        let asynchronous = self
            .location
            .ancestors()
            .into_iter()
            .find_map(|ancestor| {
                if let Some(function) = ast::Fn::cast(ancestor.clone()) {
                    Some(function.async_token().is_some())
                } else if let Some(closure) = ast::ClosureExpr::cast(ancestor.clone()) {
                    Some(closure.async_token().is_some())
                } else {
                    ast::BlockExpr::cast(ancestor)
                        .filter(|block| block.async_token().is_some())
                        .map(|_| true)
                }
            })
            .unwrap_or(false);
        let location = self.location.clone();
        self.edit(|editor, node, edition| {
            let helper = fragment::path(helper, edition)?;
            let mut inputs = context
                .iter()
                .map(|text| fragment::expression(text, edition))
                .collect::<Result<Vec<_>>>()?;
            if region.is_none()
                && let Some(arguments) = arguments(node)
            {
                let generics = if let Some(method) = ast::MethodCallExpr::cast(node.clone()) {
                    inputs.push(method.receiver().ok_or("method call has no receiver")?);
                    method.generic_arg_list()
                } else {
                    ast::CallExpr::cast(node.clone())
                        .and_then(|call| call.expr())
                        .and_then(|expression| match expression {
                            ast::Expr::PathExpr(path) => path.path(),
                            _ => None,
                        })
                        .and_then(|path| path.segment())
                        .and_then(|segment| segment.generic_arg_list())
                };
                inputs.extend(arguments.args());
                let helper = if let Some(generics) = generics {
                    fragment::expression(
                        &format!("{}{}", helper.syntax(), generics.syntax()),
                        edition,
                    )?
                } else {
                    make::expr_path(helper)
                };
                editor.replace(
                    node,
                    make::expr_call(helper, make::arg_list(inputs)).syntax(),
                );
            } else if region.is_none()
                && let Some(closure) = ast::ClosureExpr::cast(node.clone())
            {
                inputs.push(ast::Expr::ClosureExpr(closure));
                editor.replace(
                    node,
                    make::expr_call(make::expr_path(helper), make::arg_list(inputs)).syntax(),
                );
            } else if region.is_some() || ast::Fn::can_cast(node.kind()) {
                let contents = if let Some(region) = &region {
                    flow::check(&location, region, region::tail(node, region))?;
                    format!(
                        "{{{}}}",
                        region.iter().map(ToString::to_string).collect::<String>()
                    )
                } else {
                    ast::Fn::cast(node.clone())
                        .and_then(|function| function.body())
                        .ok_or("function has no body")?
                        .syntax()
                        .to_string()
                };
                let wrapped = if asynchronous {
                    format!("async {contents}")
                } else {
                    format!("|| {contents}")
                };
                inputs.push(fragment::expression(&wrapped, edition)?);
                let call: ast::Expr =
                    make::expr_call(make::expr_path(helper), make::arg_list(inputs)).into();
                let call = if asynchronous {
                    make::expr_await(call)
                } else {
                    call
                };
                if let Some(region) = region {
                    let first = region
                        .iter()
                        .find(|element| element.kind() != SyntaxKind::WHITESPACE)
                        .ok_or("selected region is empty")?;
                    let last = region
                        .iter()
                        .rfind(|element| element.kind() != SyntaxKind::WHITESPACE)
                        .ok_or("selected region is empty")?;
                    let replacement = if region::expression(node, &region) {
                        call.syntax().clone()
                    } else {
                        make::expr_stmt(call).syntax().clone()
                    };
                    editor.replace_all(first.clone()..=last.clone(), vec![replacement.into()]);
                } else {
                    let body = ast::Fn::cast(node.clone())
                        .and_then(|function| function.body())
                        .ok_or("function has no body")?;
                    editor.replace(
                        body.syntax(),
                        make::block_expr(std::iter::empty(), Some(call)).syntax(),
                    );
                }
            } else {
                return Err("delegation requires a call, closure or function".into());
            }
            Ok(())
        })
    }

    pub fn extract(mut self, signature: &str, arguments: &[&str]) -> Result<()> {
        let function = self
            .location
            .ancestors()
            .into_iter()
            .find_map(ast::Fn::cast)
            .ok_or("extraction requires an enclosing function")?;
        let anchor = self.location.at(function.syntax().clone()).range().start();
        let selected_region = self.location.region.take();
        let location = self.location.clone();
        let mut source = self.source.clone();
        let (generated, declaration) = Selected {
            source: &mut source,
            location: self.location,
            flow: flow::Options::default(),
            position: self.position,
        }
        .edit(|editor, node, edition| {
            let target = ast::MatchArm::cast(node.clone())
                .and_then(|arm| arm.expr())
                .map_or_else(|| node.clone(), |expression| expression.syntax().clone());
            let node = &target;
            let closure_body = node
                .parent()
                .and_then(ast::ClosureExpr::cast)
                .and_then(|closure| closure.body())
                .is_some_and(|body| body.syntax() == node);
            let generated = fragment::signature(signature, edition)?;
            let name = generated.name().ok_or("extracted function has no name")?;
            let params = generated
                .param_list()
                .ok_or("extracted function has no parameters")?;
            if params.params().count() != arguments.len() {
                return Err("signature and call have different argument counts".into());
            }
            let inputs = arguments
                .iter()
                .map(|text| fragment::expression(text, edition))
                .collect::<Result<Vec<_>>>()?;
            let call: ast::Expr = if params.self_param().is_some() {
                make::expr_method_call(
                    fragment::expression("self", edition)?,
                    make::name_ref(name.text()),
                    make::arg_list(inputs),
                )
                .into()
            } else {
                make::expr_call(
                    make::expr_path(fragment::path(name.text(), edition)?),
                    make::arg_list(inputs),
                )
                .into()
            };
            let call = if generated.async_token().is_some() {
                make::expr_await(call)
            } else {
                call
            };
            let function_body = function
                .body()
                .and_then(|body| body.stmt_list())
                .ok_or("function has no body")?;
            let (region, tail, expression) = if let Some(region) = selected_region {
                let tail = region::tail(node, &region);
                let expression = region::expression(node, &region);
                (region, tail, expression)
            } else if node == function.syntax() {
                (block_contents(&function_body)?, true, true)
            } else if let Some(block) = ast::BlockExpr::cast(node.clone())
                && block.modifier().is_none()
            {
                let list = block.stmt_list().ok_or("block has no statements")?;
                let tail = closure_body
                    || block.syntax() == function.body().ok_or("function has no body")?.syntax();
                (
                    block_contents(&list)?,
                    tail,
                    tail || list.tail_expr().is_some(),
                )
            } else {
                let statement = node
                    .parent()
                    .filter(|parent| ast::ExprStmt::can_cast(parent.kind()))
                    .unwrap_or_else(|| node.clone());
                let expression = ast::Expr::can_cast(statement.kind());
                (vec![statement.into()], closure_body, expression)
            };
            let first = region
                .iter()
                .find(|element| element.kind() != SyntaxKind::WHITESPACE)
                .ok_or("selected region is empty")?;
            let last = region
                .iter()
                .rfind(|element| element.kind() != SyntaxKind::WHITESPACE)
                .ok_or("selected region is empty")?;
            let transformed = flow::extract(
                self.source,
                &location,
                &region,
                tail,
                &generated,
                call,
                self.flow,
            )?;
            let call = transformed.call;
            let (body_editor, generated) = SyntaxEditor::with_ast_node(&generated);
            body_editor.replace(
                generated.body().ok_or("missing generated body")?.syntax(),
                transformed.body.syntax(),
            );
            if let Some(ty) = transformed.return_type {
                let signature = fragment::signature(&format!("fn f() -> {ty}"), edition)?;
                let ret = signature.ret_type().ok_or("missing return type")?;
                if let Some(old) = generated.ret_type() {
                    body_editor.replace(old.syntax(), ret.syntax());
                } else {
                    body_editor.insert_all(
                        Position::after(
                            generated.param_list().ok_or("missing parameters")?.syntax(),
                        ),
                        vec![whitespace(" "), ret.syntax().clone().into()],
                    );
                }
            }
            let generated = body_editor.finish().new_root().clone();
            let replacement: SyntaxElement = if expression {
                call.syntax().clone().into()
            } else {
                make::expr_stmt(call).syntax().clone().into()
            };
            editor.replace_all(first.clone()..=last.clone(), vec![replacement]);
            Ok((generated, transformed.declaration))
        })?;
        let mut root = Location::root(source.root.clone(), source.edition);
        root.module = source.module.clone();
        let location = root
            .descendants()
            .into_iter()
            .find(|location| {
                ast::Fn::can_cast(location.node.kind()) && location.range().start() == anchor
            })
            .ok_or("enclosing function is missing after extraction")?;
        let owner = location
            .ancestors()
            .into_iter()
            .find(|node| {
                matches!(
                    node.kind(),
                    SyntaxKind::FN | SyntaxKind::IMPL | SyntaxKind::TRAIT
                ) && node.parent().is_some_and(|parent| {
                    matches!(
                        parent.kind(),
                        SyntaxKind::SOURCE_FILE | SyntaxKind::ITEM_LIST | SyntaxKind::STMT_LIST
                    )
                }) && location.at(node.clone()).parents.iter().all(|frame| {
                    !frame
                        .tree
                        .syntax()
                        .ancestors()
                        .any(|ancestor| matches!(ancestor.kind(), SyntaxKind::ASSOC_ITEM_LIST))
                })
            })
            .map(|node| location.at(node));
        Selected {
            source: &mut source,
            location,
            flow: flow::Options::default(),
            position: None,
        }
        .edit(|editor, node, _| {
            let indentation = ast::edit::IndentLevel::from_node(node);
            editor.insert_all(
                Position::after(node),
                vec![whitespace(&format!("\n\n{indentation}")), generated.into()],
            );
            Ok(())
        })?;
        if let Some(declaration) = declaration {
            let owner = owner.ok_or("extracted control-flow type has no declaration scope")?;
            let mut root = Location::root(source.root.clone(), source.edition);
            root.module = source.module.clone();
            let location = root
                .descendants()
                .into_iter()
                .find(|location| {
                    location.node.kind() == owner.node.kind()
                        && location.range().start() == owner.range().start()
                })
                .ok_or("control-flow declaration scope is missing after extraction")?;
            let name = declaration.name().ok_or("control-flow type has no name")?;
            let scope = location.node.parent().ok_or("declaration has no parent")?;
            if location.at(scope).descendants().iter().any(|candidate| {
                candidate
                    .node
                    .children()
                    .find_map(ast::Name::cast)
                    .is_some_and(|existing| existing.text() == name.text())
            }) {
                return Err(format!("control-flow type `{}` already exists", name.text()).into());
            }
            Selected {
                source: &mut source,
                location,
                flow: flow::Options::default(),
                position: None,
            }
            .edit(|editor, node, _| {
                let indentation = ast::edit::IndentLevel::from_node(node);
                editor.insert_all(
                    Position::before(node),
                    vec![
                        declaration.syntax().clone().into(),
                        whitespace(&format!("\n\n{indentation}")),
                    ],
                );
                Ok(())
            })?;
        }
        self.source.root = source.root;
        Ok(())
    }
}

fn import_branch(tree: &ast::UseTree) -> Result<SyntaxNode> {
    let mut branch = tree.clone();
    while let Some(list) = branch.syntax().parent().and_then(ast::UseTreeList::cast) {
        if list.use_trees().count() != 1 {
            return Ok(branch.syntax().clone());
        }
        branch = list
            .syntax()
            .parent()
            .and_then(ast::UseTree::cast)
            .ok_or("import list has no owner")?;
    }
    branch
        .syntax()
        .parent()
        .filter(|node| ast::Use::can_cast(node.kind()))
        .ok_or_else(|| "import has no declaration".into())
}

fn whitespace(text: &str) -> SyntaxElement {
    make::tokens::whitespace(text).into()
}

fn header(node: &SyntaxNode) -> Result<SyntaxElement> {
    node.children_with_tokens()
        .find(|element| {
            !element.kind().is_trivia()
                && element
                    .as_node()
                    .is_none_or(|node| !ast::AnyAttr::can_cast(node.kind()))
        })
        .ok_or_else(|| "declaration has no header".into())
}

fn insertion(list: &SyntaxNode, (location, edge): &(Location, Edge)) -> Result<usize> {
    let contents = list.children_with_tokens().collect::<Vec<_>>();
    if matches!(edge, Edge::Start | Edge::End) {
        if location.region.is_none()
            && (location.node == *list
                || list.parent().as_ref() == Some(&location.node)
                || crate::select::body(&location.node)
                    .is_some_and(|body| Some(body) == list.parent()))
        {
            return Ok(if matches!(edge, Edge::Start) {
                usize::from(
                    contents
                        .first()
                        .is_some_and(|element| matches!(element.kind(), T!['{'] | T!['('] | T![<])),
                )
            } else {
                contents.len()
                    - usize::from(
                        contents.last().is_some_and(|element| {
                            matches!(element.kind(), T!['}'] | T![')'] | T![>])
                        }),
                    )
            });
        }
        return Err("insertion boundary does not belong to the member list".into());
    }
    let after = matches!(edge, Edge::After);
    let element = if let Some(region) = &location.region {
        if after { region.last() } else { region.first() }
            .cloned()
            .ok_or("empty insertion boundary")?
    } else {
        location.node.clone().into()
    };
    let index = contents
        .iter()
        .position(|candidate| candidate == &element)
        .ok_or("insertion boundary is not a member of the selected object")?;
    if !after {
        return Ok(index);
    }
    Ok(contents
        .iter()
        .enumerate()
        .skip(index + 1)
        .find(|(_, element)| !element.kind().is_trivia() && element.kind() != T![,])
        .map_or(contents.len(), |(index, _)| index))
}

fn append(
    editor: &SyntaxEditor,
    list: &SyntaxNode,
    element: SyntaxElement,
    position: Option<&(Location, Edge)>,
) -> Result<()> {
    let contents = list.children_with_tokens().collect::<Vec<_>>();
    let end = if let Some(position) = position {
        insertion(list, position)?
    } else {
        contents
            .iter()
            .position(|element| element.kind() == T![..])
            .unwrap_or(
                contents
                    .len()
                    .checked_sub(1)
                    .ok_or("list has no delimiter")?,
            )
    };
    let before = &contents[..end];
    let last = before
        .iter()
        .rposition(|element| !element.kind().is_trivia())
        .ok_or("list has no opening delimiter")?;
    let occupied = before.iter().any(|element| element.as_node().is_some());
    let comma = |element: &SyntaxElement| {
        element.kind() == T![,]
            || element
                .as_node()
                .and_then(SyntaxNode::last_token)
                .is_some_and(|token| token.kind() == T![,])
    };
    let separator = occupied && !comma(&before[last]);
    let spacing = if list.to_string().contains('\n') {
        format!("\n{}", ast::edit::IndentLevel::from_node(list) + 1)
    } else {
        " ".to_owned()
    };
    let trailing = !comma(&element);
    let mut elements = vec![whitespace(&spacing), element];
    if trailing {
        elements.push(make::token(T![,]).into());
    }
    if before[last + 1..]
        .iter()
        .any(|element| element.kind() == SyntaxKind::COMMENT)
    {
        if separator {
            editor.insert(Position::after(before[last].clone()), make::token(T![,]));
        }
        let position = before
            .last()
            .filter(|element| element.kind() == SyntaxKind::WHITESPACE)
            .unwrap_or(&contents[end]);
        editor.insert_all(Position::before(position.clone()), elements);
    } else {
        if separator {
            elements.insert(0, make::token(T![,]).into());
        }
        editor.insert_all(Position::after(before[last].clone()), elements);
    }
    Ok(())
}

fn insert_item(
    editor: &SyntaxEditor,
    node: &SyntaxNode,
    item: &SyntaxNode,
    position: Option<&(Location, Edge)>,
) -> Result<()> {
    let list = if node.kind() == SyntaxKind::SOURCE_FILE {
        node.clone()
    } else {
        crate::select::body(node)
            .unwrap_or_else(|| node.clone())
            .children()
            .find(|node| {
                matches!(
                    node.kind(),
                    SyntaxKind::ITEM_LIST | SyntaxKind::ASSOC_ITEM_LIST | SyntaxKind::STMT_LIST
                )
            })
            .ok_or("selected object cannot contain items")?
    };
    let position = if let Some(position) = position {
        let index = insertion(&list, position)?;
        list.children_with_tokens()
            .nth(index)
            .map_or_else(|| Position::last_child_of(&list), Position::before)
    } else if list.kind() == SyntaxKind::SOURCE_FILE {
        Position::last_child_of(&list)
    } else {
        Position::before(
            list.last_token()
                .ok_or("item list has no closing delimiter")?,
        )
    };
    editor.insert_all(
        position,
        vec![whitespace("\n"), item.clone().into(), whitespace("\n")],
    );
    Ok(())
}

fn block_contents(list: &ast::StmtList) -> Result<Vec<SyntaxElement>> {
    let open = list.l_curly_token().ok_or("block has no opening brace")?;
    let close = list.r_curly_token().ok_or("block has no closing brace")?;
    Ok(list
        .syntax()
        .children_with_tokens()
        .skip_while(|element| element != &SyntaxElement::Token(open.clone()))
        .skip(1)
        .take_while(|element| element != &SyntaxElement::Token(close.clone()))
        .collect())
}
