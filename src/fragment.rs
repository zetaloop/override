use ra_ap_syntax::{
    AstNode, Edition, SourceFile, SyntaxKind, SyntaxNode, ast,
    ast::{HasName, make},
    syntax_editor::SyntaxEditor,
};

use crate::Result;

pub(crate) fn file(text: &str, edition: Edition) -> Result<SyntaxNode> {
    let parsed = SourceFile::parse(text, edition);
    if !parsed.errors().is_empty() {
        return Err(format!("invalid Rust syntax: {:?}", parsed.errors()).into());
    }
    Ok(parsed.syntax_node())
}

pub(crate) fn expression(text: &str, edition: Edition) -> Result<ast::Expr> {
    let parsed = ast::Expr::parse(text, edition);
    if !parsed.errors().is_empty() {
        return Err(format!("invalid expression `{text}`: {:?}", parsed.errors()).into());
    }
    Ok(parsed.tree())
}

pub(crate) fn one<N: AstNode>(root: &SyntaxNode) -> Result<N> {
    if root.kind() == ra_ap_syntax::SyntaxKind::SOURCE_FILE
        && root
            .children()
            .filter(|node| ast::Item::can_cast(node.kind()))
            .count()
            != 1
    {
        return Err("fragment contains extra declarations".into());
    }
    let mut nodes = root.descendants().filter_map(N::cast).filter(|node| {
        if node.syntax() == root {
            return true;
        }
        for parent in node.syntax().ancestors().skip(1) {
            if N::can_cast(parent.kind()) {
                return false;
            }
            if &parent == root {
                break;
            }
        }
        true
    });
    let node = nodes.next().ok_or("fragment has no matching syntax node")?;
    if nodes.next().is_some() {
        return Err("fragment contains more than one matching syntax node".into());
    }
    Ok(node)
}

pub(crate) fn name(text: &str, edition: Edition) -> Result<ast::Name> {
    let root = file(&format!("struct {text};"), edition)?;
    let structure = one::<ast::Struct>(&root)?;
    if root.children().count() != 1 || structure.syntax().children().count() != 1 {
        return Err("expected an identifier".into());
    }
    structure.name().ok_or_else(|| "missing name".into())
}

pub(crate) fn ty(text: &str, edition: Edition) -> Result<ast::Type> {
    let root = file(&format!("type T = {text};"), edition)?;
    let alias = one::<ast::TypeAlias>(&root)?;
    if root.children().count() != 1 || alias.syntax().children().count() != 2 {
        return Err("expected a type".into());
    }
    alias.ty().ok_or_else(|| "missing type".into())
}

pub(crate) fn signature(text: &str, edition: Edition) -> Result<ast::Fn> {
    let root = file(&format!("{text} {{}}"), edition)?;
    let function = one::<ast::Fn>(&root)?;
    if root.children().count() != 1 {
        return Err("expected one function signature".into());
    }
    Ok(function)
}

pub(crate) fn path(text: &str, edition: Edition) -> Result<ast::Path> {
    match expression(text, edition)? {
        ast::Expr::PathExpr(path) => path.path().ok_or_else(|| "missing path".into()),
        _ => Err(format!("expected a path, got `{text}`").into()),
    }
}

pub(crate) fn token_tree(
    body: &str,
    original: &ast::TokenTree,
    edition: Edition,
) -> Result<ast::TokenTree> {
    let open = original
        .syntax()
        .first_token()
        .ok_or("missing macro delimiter")?;
    let close = original
        .syntax()
        .last_token()
        .ok_or("missing macro delimiter")?;
    let expression = expression(&format!("m!{}{body}{}", open.text(), close.text()), edition)?;
    expression
        .syntax()
        .descendants()
        .find_map(ast::MacroCall::cast)
        .and_then(|call| call.token_tree())
        .ok_or_else(|| "missing macro token tree".into())
}

pub(crate) fn equivalent(left: &SyntaxNode, right: &SyntaxNode) -> bool {
    let tokens = |node: &SyntaxNode| {
        node.descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| !token.kind().is_trivia())
            .map(|token| (token.kind(), token.text().to_owned()))
    };
    left.kind() == right.kind() && tokens(left).eq(tokens(right))
}

pub(crate) fn block(
    elements: &[ra_ap_syntax::SyntaxElement],
    edition: Edition,
) -> Result<ast::Expr> {
    let start = elements
        .iter()
        .position(|element| element.kind() != SyntaxKind::WHITESPACE)
        .unwrap_or(elements.len());
    let end = elements
        .iter()
        .rposition(|element| element.kind() != SyntaxKind::WHITESPACE)
        .map_or(start, |index| index + 1);
    let mut text = String::from("{\n    ");
    for element in &elements[start..end] {
        if let Some(node) = element.as_node() {
            text.push_str(&indent(node, "    ").to_string());
        } else if element.kind() == SyntaxKind::WHITESPACE && element.to_string().contains('\n') {
            text.push_str(&format!(
                "{}    ",
                "\n".repeat(element.to_string().matches('\n').count())
            ));
        } else {
            text.push_str(&element.to_string());
        }
    }
    text.push_str("\n}");
    expression(&text, edition)
}

pub(crate) fn indentation(node: &SyntaxNode) -> String {
    std::iter::successors(node.first_token(), |token| token.prev_token())
        .filter(|token| token.kind() == SyntaxKind::WHITESPACE)
        .find_map(|token| {
            token
                .text()
                .rsplit_once('\n')
                .map(|(_, indent)| indent.to_owned())
        })
        .unwrap_or_default()
}

pub(crate) fn indent(node: &SyntaxNode, indentation: &str) -> SyntaxNode {
    let original = self::indentation(node);
    let (editor, root) = SyntaxEditor::new(node.clone());
    for token in root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        if token.kind() == SyntaxKind::WHITESPACE
            && let Some((lines, indent)) = token.text().rsplit_once('\n')
        {
            let lines = lines
                .split('\n')
                .map(|line| line.trim_end_matches([' ', '\t']))
                .collect::<Vec<_>>()
                .join("\n");
            let indent = indent.strip_prefix(&original).map_or_else(
                || indent.to_owned(),
                |relative| format!("{indentation}{relative}"),
            );
            editor.replace(
                token,
                make::tokens::whitespace(&format!("{lines}\n{indent}")),
            );
        }
    }
    editor.finish().new_root().clone()
}

pub(crate) fn spelling(node: &SyntaxNode) -> String {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| !token.kind().is_trivia())
        .map(|token| token.text().to_owned())
        .collect()
}
