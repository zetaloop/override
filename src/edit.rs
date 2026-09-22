use std::path::Path;

use ra_ap_syntax::{
    AstNode, SyntaxElement, SyntaxKind, SyntaxNode, T,
    ast::{self, HasGenericArgs, HasName, HasVisibility, make},
    syntax_editor::{Position, Removable, SyntaxEditor},
};

use crate::{Result, Selected, fragment, resolve, select::arguments};

impl Selected<'_> {
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

    pub fn add_attribute(self, attribute: &str) -> Result<()> {
        self.edit(|editor, node, edition| {
            let root = fragment::file(&format!("{attribute}\nstruct S;"), edition)?;
            let attribute = fragment::one::<ast::AnyAttr>(&root)?;
            editor.insert_all(
                Position::first_child_of(node),
                vec![attribute.syntax().clone().into(), whitespace("\n")],
            );
            Ok(())
        })
    }

    pub fn add_field(self, declaration: &str) -> Result<()> {
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
            append(editor, &list, field.into())
        })
    }

    pub fn add_variant(self, declaration: &str) -> Result<()> {
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
            )
        })
    }

    pub fn add_parameter(self, declaration: &str) -> Result<()> {
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
            if ast::SelfParam::can_cast(parameter.kind()) {
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
                append(editor, list.syntax(), parameter.clone().into())
            }
        })
    }

    pub fn add_generic(self, declaration: &str) -> Result<()> {
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
                append(editor, existing.syntax(), parameter.syntax().clone().into())
            } else {
                let name = node
                    .children()
                    .find_map(ast::Name::cast)
                    .ok_or("selected object has no name")?;
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

    pub fn add_argument(self, value: &str) -> Result<()> {
        self.edit(|editor, node, edition| {
            let list = arguments(node).ok_or("selected object is not a call")?;
            append(
                editor,
                list.syntax(),
                fragment::expression(value, edition)?
                    .syntax()
                    .clone()
                    .into(),
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

    pub fn add_use(self, declaration: &str) -> Result<()> {
        self.edit(|editor, node, edition| {
            let parsed = fragment::file(declaration, edition)?;
            let import = fragment::one::<ast::Use>(&parsed)?;
            if parsed.children().count() != 1 {
                return Err("expected one use declaration".into());
            }
            insert_item(editor, node, import.syntax())
        })
    }

    pub fn mount_module(self, declaration: &str, path: impl AsRef<Path>) -> Result<()> {
        self.edit(|editor, node, edition| {
            let path = path.as_ref().to_str().ok_or("module path is not UTF-8")?;
            let parsed = fragment::file(&format!("#[path = {path:?}]\n{declaration};"), edition)?;
            let module = fragment::one::<ast::Module>(&parsed)?;
            if parsed.children().count() != 1 {
                return Err("expected one module declaration".into());
            }
            insert_item(editor, node, module.syntax())
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
        self.edit(|editor, node, edition| {
            let helper = fragment::path(helper, edition)?;
            let mut inputs = context
                .iter()
                .map(|text| fragment::expression(text, edition))
                .collect::<Result<Vec<_>>>()?;
            if let Some(arguments) = arguments(node) {
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
            } else if let Some(closure) = ast::ClosureExpr::cast(node.clone()) {
                inputs.push(ast::Expr::ClosureExpr(closure));
                editor.replace(
                    node,
                    make::expr_call(make::expr_path(helper), make::arg_list(inputs)).syntax(),
                );
            } else if let Some(function) = ast::Fn::cast(node.clone()) {
                let body = function.body().ok_or("function has no body")?;
                let asynchronous = function.async_token().is_some();
                let contents = region.as_ref().map_or_else(
                    || body.syntax().to_string(),
                    |region| {
                        format!(
                            "{{{}}}",
                            region.iter().map(ToString::to_string).collect::<String>()
                        )
                    },
                );
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
                    editor.replace_all(
                        first.clone()..=last.clone(),
                        vec![call.syntax().clone().into()],
                    );
                } else {
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
        let selected_region = self.location.region.take();
        self.edit(|editor, node, edition| {
            let target = ast::MatchArm::cast(node.clone()).and_then(|arm| arm.expr()).map_or_else(|| node.clone(), |expression| expression.syntax().clone());
            let node = &target;
            let function = node.ancestors().find_map(ast::Fn::cast).ok_or("extraction requires an enclosing function")?;
            let closure_body = node.parent().and_then(ast::ClosureExpr::cast).and_then(|closure| closure.body()).is_some_and(|body| body.syntax() == node);
            let generated = fragment::signature(signature, edition)?;
            let name = generated.name().ok_or("extracted function has no name")?;
            let params = generated.param_list().ok_or("extracted function has no parameters")?;
            if params.params().count() != arguments.len() { return Err("signature and call have different argument counts".into()); }
            let inputs = arguments.iter().map(|text| fragment::expression(text, edition)).collect::<Result<Vec<_>>>()?;
            let call: ast::Expr = if params.self_param().is_some() {
                make::expr_method_call(fragment::expression("self", edition)?, make::name_ref(name.text()), make::arg_list(inputs)).into()
            } else { make::expr_call(make::expr_path(fragment::path(name.text(), edition)?), make::arg_list(inputs)).into() };
            let call = if generated.async_token().is_some() { make::expr_await(call) } else { call };
            let function_body = function.body().and_then(|body| body.stmt_list()).ok_or("function has no body")?;
            let (region, tail, expression) = if let Some(region) = selected_region {
                (region, true, true)
            } else if node == function.syntax() {
                (block_contents(&function_body)?, true, true)
            } else if let Some(block) = ast::BlockExpr::cast(node.clone()) && block.modifier().is_none() {
                let list = block.stmt_list().ok_or("block has no statements")?;
                let tail = closure_body || block.syntax() == function.body().ok_or("function has no body")?.syntax();
                (block_contents(&list)?, tail, tail || list.tail_expr().is_some())
            } else {
                let statement = node.parent().filter(|parent| ast::ExprStmt::can_cast(parent.kind())).unwrap_or_else(|| node.clone());
                let expression = ast::Expr::can_cast(statement.kind());
                (vec![statement.into()], closure_body, expression)
            };
            let first = region.iter().find(|element| element.kind() != SyntaxKind::WHITESPACE).ok_or("selected region is empty")?;
            let last = region.iter().rfind(|element| element.kind() != SyntaxKind::WHITESPACE).ok_or("selected region is empty")?;
            let range = first.text_range().cover(last.text_range());
            for node in region.iter().filter_map(|element| element.as_node()).flat_map(|node| node.descendants()) {
                if node.ancestors().skip(1).take_while(|parent| range.contains_range(parent.text_range()))
                    .any(|parent| ast::ClosureExpr::can_cast(parent.kind()) || ast::Fn::can_cast(parent.kind())
                        || ast::BlockExpr::cast(parent).is_some_and(|block| block.async_token().is_some() || block.gen_token().is_some())) { continue }
                if node.kind() == SyntaxKind::TRY_EXPR && node.ancestors().skip(1).take_while(|parent| range.contains_range(parent.text_range()))
                    .filter_map(ast::BlockExpr::cast).any(|block| block.try_block_modifier().is_some()) { continue }
                if matches!(node.kind(), SyntaxKind::RETURN_EXPR | SyntaxKind::TRY_EXPR) && !tail {
                    return Err("return or ? leaves the selected region; extract its enclosing function region".into());
                }
                if matches!(node.kind(), SyntaxKind::BREAK_EXPR | SyntaxKind::CONTINUE_EXPR) {
                    let label = node.children().find_map(ast::Lifetime::cast).map(|label| label.to_string());
                    let target = node.ancestors().skip(1).find(|parent| {
                        if let Some(label) = &label { parent.children().find_map(ast::Label::cast).is_some_and(|candidate| candidate.to_string().trim_end_matches(':') == label) }
                        else { matches!(parent.kind(), SyntaxKind::FOR_EXPR | SyntaxKind::WHILE_EXPR | SyntaxKind::LOOP_EXPR) }
                    });
                    if target.is_none_or(|target| !range.contains_range(target.text_range())) { return Err("loop control leaves the selected region; extract the complete loop".into()); }
                }
            }
            let body = region.iter().map(ToString::to_string).collect::<String>();
            let body = fragment::expression(&format!("{{{body}}}"), edition)?;
            let (body_editor, generated) = SyntaxEditor::with_ast_node(&generated);
            body_editor.replace(generated.body().ok_or("missing generated body")?.syntax(), body.syntax());
            let generated = body_editor.finish().new_root().clone();
            let replacement: SyntaxElement = if expression {
                call.syntax().clone().into()
            } else { make::expr_stmt(call).syntax().clone().into() };
            editor.replace_all(first.clone()..=last.clone(), vec![replacement]);
            let indentation = ast::edit::IndentLevel::from_node(function.syntax());
            editor.insert_all(Position::after(function.syntax()), vec![whitespace(&format!("\n\n{indentation}")), generated.into()]);
            Ok(())
        })
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

fn append(editor: &SyntaxEditor, list: &SyntaxNode, element: SyntaxElement) -> Result<()> {
    let contents = list.children_with_tokens().collect::<Vec<_>>();
    let end = contents
        .iter()
        .position(|element| element.kind() == T![..])
        .unwrap_or(
            contents
                .len()
                .checked_sub(1)
                .ok_or("list has no delimiter")?,
        );
    let before = &contents[..end];
    let last = before
        .iter()
        .rposition(|element| !element.kind().is_trivia())
        .ok_or("list has no opening delimiter")?;
    let occupied = before.iter().any(|element| element.as_node().is_some());
    let separator = occupied && before[last].kind() != T![,];
    let spacing = if list.to_string().contains('\n') {
        format!("\n{}", ast::edit::IndentLevel::from_node(list) + 1)
    } else {
        " ".to_owned()
    };
    let mut elements = vec![whitespace(&spacing), element, make::token(T![,]).into()];
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

fn insert_item(editor: &SyntaxEditor, node: &SyntaxNode, item: &SyntaxNode) -> Result<()> {
    if node.kind() == SyntaxKind::SOURCE_FILE {
        editor.insert_all(
            Position::last_child_of(node),
            vec![whitespace("\n"), item.clone().into(), whitespace("\n")],
        );
    } else {
        let list = node
            .children()
            .find(|node| {
                matches!(
                    node.kind(),
                    SyntaxKind::ITEM_LIST | SyntaxKind::ASSOC_ITEM_LIST | SyntaxKind::STMT_LIST
                )
            })
            .ok_or("selected object cannot contain items")?;
        let close = list
            .last_token()
            .ok_or("item list has no closing delimiter")?;
        editor.insert_all(
            Position::before(close),
            vec![whitespace("\n"), item.clone().into(), whitespace("\n")],
        );
    }
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
