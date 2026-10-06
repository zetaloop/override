use ra_ap_syntax::{
    AstNode, SyntaxElement, SyntaxKind, SyntaxNode, TextSize, ast,
    ast::{HasName, HasVisibility, make},
    syntax_editor::{Position, SyntaxEditor},
};

use crate::{Result, Selected, Source, fragment, resolve, source::Location, symbol};

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Options {
    control_flow: bool,
    propagate: bool,
    outputs: Vec<String>,
}

impl Selected<'_> {
    pub fn control_flow(mut self) -> Self {
        self.flow.control_flow = true;
        self
    }

    pub fn propagate(mut self) -> Self {
        self.flow.propagate = true;
        self
    }

    pub fn outputs(mut self, names: impl IntoIterator<Item: Into<String>>) -> Self {
        self.flow.outputs = names.into_iter().map(Into::into).collect();
        self
    }
}

#[derive(Clone)]
enum Action {
    Return,
    Break(Option<String>),
    Continue(Option<String>),
}

impl Action {
    fn name(&self) -> String {
        match self {
            Self::Return => "Return".to_owned(),
            Self::Break(label) => {
                format!("Break{}", label.as_deref().map(camel).unwrap_or_default())
            }
            Self::Continue(label) => format!(
                "Continue{}",
                label.as_deref().map(camel).unwrap_or_default()
            ),
        }
    }

    fn expression(&self, value: Option<&str>) -> String {
        let (keyword, label) = match self {
            Self::Return => ("return", None),
            Self::Break(label) => ("break", label.as_deref()),
            Self::Continue(label) => ("continue", label.as_deref()),
        };
        format!(
            "{keyword}{}{}",
            label.map(|label| format!(" {label}")).unwrap_or_default(),
            value.map(|value| format!(" {value}")).unwrap_or_default()
        )
    }
}

struct Jump {
    location: Location,
    target: Location,
    action: Action,
}

fn exits(location: &Location, region: &[SyntaxElement]) -> Result<(Vec<Jump>, Vec<Location>)> {
    let mut scope = location.clone();
    scope.region = Some(region.to_vec());
    let range = scope.range();
    let mut jumps = Vec::new();
    let mut tries = Vec::new();
    for location in scope.descendants() {
        let node = &location.node;
        let kind = node.kind();
        if matches!(kind, SyntaxKind::RETURN_EXPR | SyntaxKind::TRY_EXPR) {
            let target = location
                .ancestors()
                .into_iter()
                .skip(1)
                .find(|node| {
                    matches!(node.kind(), SyntaxKind::FN | SyntaxKind::CLOSURE_EXPR)
                        || ast::BlockExpr::cast(node.clone()).is_some_and(|block| {
                            block.async_token().is_some()
                                || block.gen_token().is_some()
                                || (kind == SyntaxKind::TRY_EXPR
                                    && block.try_block_modifier().is_some())
                        })
                })
                .ok_or("exit has no enclosing return context")?;
            let target = location.at(target);
            if range.contains_range(target.range()) {
                continue;
            }
            if kind == SyntaxKind::TRY_EXPR {
                if !tries
                    .iter()
                    .any(|existing: &Location| existing.same(&target))
                {
                    tries.push(target);
                }
            } else {
                jumps.push(Jump {
                    location,
                    target,
                    action: Action::Return,
                });
            }
        } else if matches!(kind, SyntaxKind::BREAK_EXPR | SyntaxKind::CONTINUE_EXPR) {
            let label = node
                .children()
                .find_map(ast::Lifetime::cast)
                .map(|label| label.to_string());
            let target = location
                .ancestors()
                .into_iter()
                .skip(1)
                .take_while(|node| {
                    !matches!(node.kind(), SyntaxKind::FN | SyntaxKind::CLOSURE_EXPR)
                        && !ast::BlockExpr::cast(node.clone()).is_some_and(|block| {
                            block.async_token().is_some() || block.gen_token().is_some()
                        })
                })
                .find(|node| {
                    if let Some(label) = &label {
                        node.children()
                            .find_map(ast::Label::cast)
                            .is_some_and(|candidate| {
                                candidate.to_string().trim_end_matches(':') == label
                            })
                    } else {
                        matches!(
                            node.kind(),
                            SyntaxKind::FOR_EXPR | SyntaxKind::WHILE_EXPR | SyntaxKind::LOOP_EXPR
                        )
                    }
                })
                .ok_or("loop exit has no enclosing destination")?;
            let target = location.at(target);
            if range.contains_range(target.range()) {
                continue;
            }
            let action = if kind == SyntaxKind::BREAK_EXPR {
                Action::Break(label)
            } else {
                Action::Continue(label)
            };
            jumps.push(Jump {
                location,
                target,
                action,
            });
        }
    }
    Ok((jumps, tries))
}

pub(crate) fn check(location: &Location, region: &[SyntaxElement], tail: bool) -> Result<()> {
    let (jumps, tries) = exits(location, region)?;
    if jumps
        .iter()
        .any(|jump| !matches!(jump.action, Action::Return) || !tail)
    {
        return Err(
            "control flow leaves the selected region; enable control_flow for extraction".into(),
        );
    }
    if !tries.is_empty() && !tail {
        return Err("? leaves the selected region; enable propagate for extraction".into());
    }
    Ok(())
}

struct Exit {
    action: Action,
    target: Location,
    ty: Option<String>,
}

#[derive(Clone)]
enum Container {
    Result(String),
    Option,
    ControlFlow(String),
}

impl Container {
    fn of(source: &Source, location: &Location) -> Result<Self> {
        let ty = return_type(location)
            .ok_or("propagation requires the enclosing return type declaration")?;
        let (name, arguments) = resolve::type_shape(source, location, &ty)?;
        match (name.as_str(), arguments.as_slice()) {
            ("core::result::Result" | "std::result::Result", [_, error]) => Ok(Self::Result(error.clone())),
            ("core::option::Option" | "std::option::Option", [_]) => Ok(Self::Option),
            ("core::ops::ControlFlow" | "std::ops::ControlFlow" | "core::ops::control_flow::ControlFlow", [exit, ..]) => Ok(Self::ControlFlow(exit.clone())),
            _ => Err(format!("cannot propagate through `{ty}`; supply its Result, Option or ControlFlow declaration").into()),
        }
    }

    fn ty(&self, output: &str) -> String {
        match self {
            Self::Result(error) => format!("::core::result::Result<{output}, {error}>"),
            Self::Option => format!("::core::option::Option<{output}>"),
            Self::ControlFlow(exit) => format!("::core::ops::ControlFlow<{exit}, {output}>"),
        }
    }

    fn wrap(&self, value: &str) -> String {
        let constructor = match self {
            Self::Result(_) => "::core::result::Result::Ok",
            Self::Option => "::core::option::Option::Some",
            Self::ControlFlow(_) => "::core::ops::ControlFlow::Continue",
        };
        format!("{constructor}({value})")
    }
}

pub(crate) struct Transformed {
    pub body: ast::Expr,
    pub call: ast::Expr,
    pub binding: Option<ast::Pat>,
    pub return_type: Option<ast::Type>,
    pub declaration: Option<ast::Enum>,
}

pub(crate) fn extract(
    source: &Source,
    location: &Location,
    region: &[SyntaxElement],
    tail: bool,
    signature: &ast::Fn,
    call: ast::Expr,
    options: Options,
) -> Result<Transformed> {
    let edition = source.edition();
    let tail = tail && options.outputs.is_empty();
    let mut scope = location.clone();
    scope.region = Some(region.to_vec());
    let mut bindings: Vec<(String, ast::Pat)> = Vec::new();
    if !options.outputs.is_empty() {
        let end = region
            .last()
            .and_then(SyntaxElement::next_sibling_or_token)
            .ok_or("output bindings have no enclosing continuation")?;
        let mut context = location.at(end.parent().ok_or("output bindings have no scope")?);
        context.region = Some(vec![end]);
        for pattern in &options.outputs {
            let signature = fragment::signature(&format!("fn f({pattern}: ())"), edition)?;
            let parameter = fragment::one::<ast::Param>(signature.syntax())?;
            let binding = parameter
                .pat()
                .and_then(|pattern| ast::IdentPat::cast(pattern.syntax().clone()))
                .filter(|binding| binding.ref_token().is_none() && binding.pat().is_none())
                .ok_or("an output must be a name or mutable binding")?;
            let name = binding.name().ok_or("output binding has no name")?;
            let declaration = symbol::binding(source, &context, name.text())?;
            if !scope.range().contains_range(declaration.range()) {
                return Err(
                    format!("output `{name}` is declared outside the selected region").into(),
                );
            }
            bindings.push((name.text().to_owned(), binding.into()));
        }
    }
    let (jumps, tries) = exits(location, region)?;
    let container = if !tries.is_empty() && options.propagate {
        let [target] = tries.as_slice() else {
            return Err("selected region has several external return contexts".into());
        };
        Some(Container::of(source, target)?)
    } else {
        None
    };
    if !tries.is_empty()
        && !options.propagate
        && (!tail || options.control_flow && !jumps.is_empty())
    {
        return Err("? leaves the selected region; enable propagate".into());
    }
    if !options.control_flow
        && jumps
            .iter()
            .any(|jump| !matches!(jump.action, Action::Return) || !tail || container.is_some())
    {
        return Err("control flow leaves the selected region; enable control_flow".into());
    }
    let mut exits: Vec<Exit> = Vec::new();
    let mut edits = Vec::new();
    let origin = scope.range().start();
    let prefix = if region.first().is_some_and(|element| {
        element.kind() == SyntaxKind::WHITESPACE && element.to_string().contains('\n')
    }) {
        String::new()
    } else {
        let indentation = region
            .iter()
            .find_map(SyntaxElement::as_node)
            .map(fragment::indentation)
            .unwrap_or_default();
        format!("\n{indentation}")
    };
    if options.control_flow {
        for jump in &jumps {
            let index = if let Some(index) = exits.iter().position(|exit| {
                std::mem::discriminant(&exit.action) == std::mem::discriminant(&jump.action)
                    && exit.target.same(&jump.target)
            }) {
                index
            } else {
                let ty = match &jump.action {
                    Action::Return => {
                        let expression = ast::ReturnExpr::cast(jump.location.node.clone())
                            .and_then(|expression| expression.expr());
                        Some(
                            if let Some(ty) = return_type(&jump.target).or_else(|| {
                                expression.as_ref().and_then(|expression| {
                                    resolve::value_type(source, &jump.location, expression)
                                })
                            }) {
                                ty.to_string()
                            } else if expression.is_none() {
                                "()".to_owned()
                            } else {
                                return Err("return value needs a declared type in its enclosing signature or expression".into());
                            },
                        )
                    }
                    Action::Break(_) => {
                        let breaks = jumps
                            .iter()
                            .filter(|candidate| {
                                matches!(candidate.action, Action::Break(_))
                                    && candidate.target.same(&jump.target)
                            })
                            .map(|candidate| {
                                (
                                    candidate,
                                    ast::BreakExpr::cast(candidate.location.node.clone())
                                        .and_then(|expression| expression.expr()),
                                )
                            })
                            .collect::<Vec<_>>();
                        breaks.iter().find_map(|(candidate, expression)| expression.as_ref().map(|expression| (*candidate, expression)))
                            .map(|(candidate, expression)| {
                                if breaks.iter().any(|(_, expression)| expression.is_none()) {
                                    return Ok("()".to_owned());
                                }
                                resolve::expected_type(&candidate.target).or_else(|| resolve::value_type(source, &candidate.location, expression))
                                    .map(|ty| ty.to_string()).ok_or("break value needs a declared type in its destination or expression")
                            }).transpose()?
                    }
                    Action::Continue(_) => None,
                };
                exits.push(Exit {
                    action: jump.action.clone(),
                    target: jump.target.clone(),
                    ty,
                });
                exits.len() - 1
            };
            edits.push((
                jump.location.range().start() - origin
                    + TextSize::of(prefix.as_str())
                    + TextSize::from(1),
                jump.location.node.kind(),
                index,
            ));
        }
    }
    let contents = region.iter().map(ToString::to_string).collect::<String>();
    let body = fragment::expression(&format!("{{{prefix}{contents}}}"), edition)?;
    let mut body_source = Source {
        root: body.syntax().clone(),
        edition,
        module: String::new(),
        modules: Vec::new(),
    };
    let name = format!(
        "{}Exit",
        camel(
            signature
                .name()
                .ok_or("extracted function has no name")?
                .text()
        )
    );
    let declaration = if exits.len() > 1 {
        let parameters = exits
            .iter()
            .filter(|exit| exit.ty.is_some())
            .map(|exit| format!("{}Value", exit.action.name()))
            .collect::<Vec<_>>();
        let generics = if parameters.is_empty() {
            String::new()
        } else {
            format!("<{}>", parameters.join(", "))
        };
        let variants = exits
            .iter()
            .map(|exit| {
                let variant = exit.action.name();
                if exit.ty.is_some() {
                    format!("{variant}({variant}Value)")
                } else {
                    variant
                }
            })
            .collect::<Vec<_>>()
            .join(",\n    ");
        let visibility = signature
            .visibility()
            .map(|visibility| format!("{visibility} "))
            .unwrap_or_default();
        Some(fragment::one::<ast::Enum>(&fragment::file(
            &format!("{visibility}enum {name}{generics} {{\n    {variants},\n}}"),
            edition,
        )?)?)
    } else {
        None
    };
    edits.sort_by_key(|(offset, _, _)| std::cmp::Reverse(*offset));
    for (offset, kind, index) in edits {
        let root = Location::root(body_source.root.clone(), edition);
        let location = root
            .descendants()
            .into_iter()
            .find(|location| location.range().start() == offset && location.node.kind() == kind)
            .ok_or("control-flow expression is missing during extraction")?;
        let exit = &exits[index];
        Selected {
            source: &mut body_source,
            location,
            flow: Options::default(),
            position: None,
        }
        .edit(|editor, node, edition| {
            let value = ast::ReturnExpr::cast(node.clone())
                .and_then(|expression| expression.expr())
                .or_else(|| {
                    ast::BreakExpr::cast(node.clone()).and_then(|expression| expression.expr())
                })
                .map(|expression| expression.to_string())
                .unwrap_or_else(|| "()".to_owned());
            let value = if declaration.is_some() {
                let variant = format!("{name}::{}", exit.action.name());
                if exit.ty.is_some() {
                    format!("{variant}({value})")
                } else {
                    variant
                }
            } else {
                value
            };
            let value = format!("::core::ops::ControlFlow::Break({value})");
            let value = container
                .as_ref()
                .map_or_else(|| value.clone(), |container| container.wrap(&value));
            editor.replace(
                node,
                fragment::expression(&format!("return {value}"), edition)?.syntax(),
            );
            Ok(())
        })?;
    }
    let elements = body_source
        .root
        .children()
        .find_map(ast::StmtList::cast)
        .ok_or("extracted body has no statements")?
        .syntax()
        .children_with_tokens()
        .skip(1)
        .take_while(|element| element.kind() != ra_ap_syntax::T!['}'])
        .collect::<Vec<_>>();
    body_source.root = fragment::block(&elements, edition)?.syntax().clone();
    let mut output = signature
        .ret_type()
        .and_then(|ret| ret.ty())
        .map(|ty| ty.to_string())
        .unwrap_or_else(|| "()".to_owned());
    let block =
        ast::BlockExpr::cast(body_source.root.clone()).ok_or("extracted body is not a block")?;
    let list = block
        .stmt_list()
        .ok_or("extracted body has no statements")?;
    let mut value = list
        .tail_expr()
        .map(|expression| fragment::indent(expression.syntax(), "").to_string())
        .unwrap_or_else(|| "()".to_owned());
    let result = region
        .iter()
        .rfind(|element| !element.kind().is_trivia())
        .and_then(SyntaxElement::as_node)
        .is_some_and(|node| ast::Expr::can_cast(node.kind()) && !diverges(node));
    if !bindings.is_empty() {
        let mut values = Vec::new();
        if result {
            values.push(fragment::expression(&value, edition)?);
        }
        values.extend(
            bindings
                .iter()
                .map(|(name, _)| fragment::expression(name, edition))
                .collect::<Result<Vec<_>>>()?,
        );
        value = match values.as_slice() {
            [value] => value.to_string(),
            _ => make::expr_tuple(values).to_string(),
        };
    }
    let mut call = call.to_string();
    if !exits.is_empty() {
        let ty = if declaration.is_some() {
            let parameters = exits
                .iter()
                .filter_map(|exit| exit.ty.as_ref())
                .cloned()
                .collect::<Vec<_>>();
            if parameters.is_empty() {
                name.clone()
            } else {
                format!("{name}<{}>", parameters.join(", "))
            }
        } else {
            exits[0].ty.clone().unwrap_or_else(|| "()".to_owned())
        };
        output = format!("::core::ops::ControlFlow<{ty}, {output}>");
        value = format!("::core::ops::ControlFlow::Continue({value})");
    }
    if let Some(container) = &container {
        output = container.ty(&output);
        value = container.wrap(&value);
        call.push('?');
    }
    if !exits.is_empty() {
        let mut arms = vec!["::core::ops::ControlFlow::Continue(value) => value".to_owned()];
        for exit in &exits {
            let binding = exit.ty.as_ref().map(|_| "value");
            let pattern = if declaration.is_some() {
                let variant = format!("{name}::{}", exit.action.name());
                if binding.is_some() {
                    format!("{variant}(value)")
                } else {
                    variant
                }
            } else if binding.is_some() {
                "value".to_owned()
            } else {
                "()".to_owned()
            };
            arms.push(format!(
                "::core::ops::ControlFlow::Break({pattern}) => {}",
                exit.action.expression(binding)
            ));
        }
        call = format!("match {call} {{\n    {},\n}}", arms.join(",\n    "));
    }
    let transformed = !exits.is_empty() || container.is_some();
    if (transformed || !bindings.is_empty()) && !diverges(block.syntax()) {
        let (editor, _) = SyntaxEditor::new(body_source.root.clone());
        let value = fragment::expression(&value, edition)?;
        let value = fragment::indent(value.syntax(), "    ");
        if let Some(tail) = list.tail_expr()
            && (bindings.is_empty() || result)
        {
            editor.replace(tail.syntax(), value);
        } else {
            if let Some(tail) = list.tail_expr() {
                editor.replace(tail.syntax(), make::expr_stmt(tail.clone()).syntax());
            }
            let close = list
                .r_curly_token()
                .ok_or("extracted body has no closing brace")?;
            if let Some(space) = close
                .prev_sibling_or_token()
                .filter(|element| element.kind() == SyntaxKind::WHITESPACE)
            {
                editor.delete(space);
            }
            editor.insert_all(
                Position::before(close),
                vec![
                    make::tokens::whitespace("\n    ").into(),
                    value.into(),
                    make::tokens::whitespace("\n").into(),
                ],
            );
        }
        body_source.root = editor.finish().new_root().clone();
    }
    let binding = if bindings.is_empty() {
        None
    } else if result {
        let patterns = bindings
            .iter()
            .map(|(name, _)| format!("_{}", name.trim_start_matches("r#")))
            .collect::<Vec<_>>()
            .join(", ");
        call = format!("match {call} {{ (value, {patterns}) => value }}");
        None
    } else {
        Some(match bindings.as_slice() {
            [(_, binding)] => binding.clone(),
            _ => make::tuple_pat(bindings.into_iter().map(|(_, binding)| binding)).into(),
        })
    };
    let body = ast::Expr::cast(body_source.root).ok_or("extracted body is not an expression")?;
    Ok(Transformed {
        body,
        call: fragment::expression(&call, edition)?,
        binding,
        return_type: transformed
            .then(|| fragment::ty(&output, edition))
            .transpose()?,
        declaration,
    })
}

fn diverges(node: &SyntaxNode) -> bool {
    match node.kind() {
        SyntaxKind::RETURN_EXPR | SyntaxKind::BREAK_EXPR | SyntaxKind::CONTINUE_EXPR => true,
        SyntaxKind::STMT_LIST | SyntaxKind::EXPR_STMT => {
            node.children().last().is_some_and(|node| diverges(&node))
        }
        SyntaxKind::BLOCK_EXPR => ast::BlockExpr::cast(node.clone()).is_some_and(|block| {
            block.modifier().is_none()
                && block
                    .stmt_list()
                    .is_some_and(|list| diverges(list.syntax()))
        }),
        SyntaxKind::IF_EXPR => ast::IfExpr::cast(node.clone()).is_some_and(|expression| {
            expression
                .then_branch()
                .is_some_and(|branch| diverges(branch.syntax()))
                && expression.else_branch().is_some_and(|branch| match branch {
                    ast::ElseBranch::Block(block) => diverges(block.syntax()),
                    ast::ElseBranch::IfExpr(expression) => diverges(expression.syntax()),
                })
        }),
        SyntaxKind::MATCH_EXPR => ast::MatchExpr::cast(node.clone())
            .and_then(|expression| expression.match_arm_list())
            .is_some_and(|list| {
                list.arms().next().is_some()
                    && list.arms().all(|arm| {
                        arm.expr()
                            .is_some_and(|expression| diverges(expression.syntax()))
                    })
            }),
        _ => false,
    }
}

fn return_type(location: &Location) -> Option<ast::Type> {
    if let Some(function) = ast::Fn::cast(location.node.clone()) {
        function
            .ret_type()
            .and_then(|ret| ret.ty())
            .or_else(|| fragment::ty("()", location.edition).ok())
    } else if let Some(closure) = ast::ClosureExpr::cast(location.node.clone()) {
        closure.ret_type().and_then(|ret| ret.ty())
    } else {
        resolve::expected_type(location)
    }
}

fn camel(name: &str) -> String {
    name.trim_start_matches('\'')
        .trim_start_matches("r#")
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().chain(characters).collect::<String>())
                .unwrap_or_default()
        })
        .collect()
}
