use std::collections::HashSet;

use format::{Count, ParseMode, Parser, Piece, Position};
use ra_ap_syntax::{ast, ast::HasName};
use span::Span;
use tt::{DelimiterKind, TopSubtree, TopSubtreeBuilder};

use crate::Result;

pub(super) fn captures(
    format: &ast::FormatArgsExpr,
    mut value: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let Some(ast::Expr::Literal(literal)) = format.template() else {
        return Ok(());
    };
    let ast::LiteralKind::String(template) = literal.kind() else {
        return Ok(());
    };
    let text = template
        .value()
        .map_err(|e| format!("invalid format string: {e:?}"))?;
    let mut names: HashSet<String> = format
        .args()
        .filter_map(|arg| arg.name())
        .map(|name| crate::resolve::ident(name.text()).to_owned())
        .collect();
    let mut capture = |name: &str| -> Result<()> {
        if names.insert(name.to_owned()) {
            value(name)?;
        }
        Ok(())
    };
    let mut parser = Parser::new(&text, None, None, false, ParseMode::Format);
    for piece in parser.by_ref() {
        if let Piece::NextArgument(arg) = piece {
            if let Position::ArgumentNamed(name) = arg.position {
                capture(name)?;
            }
            for count in [arg.format.width, arg.format.precision] {
                if let Count::CountIsName(name, _) = count {
                    capture(name)?;
                }
            }
        }
    }
    if let Some(error) = parser.errors.first() {
        return Err(error.description.clone().into());
    }
    Ok(())
}

pub(super) fn flow(
    db: &salsa::DatabaseImpl,
    name: &str,
    args: &TopSubtree,
    span: Span,
) -> Result<TopSubtree> {
    let mut out = TopSubtreeBuilder::new(tt::Delimiter::invisible_spanned(span));
    match name {
        "stringify" | "concat" | "concat_bytes" | "file" | "line" | "column" | "module_path"
        | "cfg" | "env" | "option_env" | "include_str" | "include_bytes" | "compile_error"
        | "log_syntax" | "trace_macros" => {
            out.open(DelimiterKind::Parenthesis, span);
            out.close(span);
        }
        "format_args" | "format_args_nl" | "const_format_args" => {
            out.push(tt::Ident::new("k#format_args", span).into());
            out.open(DelimiterKind::Parenthesis, span);
            out.extend_with_tt(args.token_trees());
            out.close(span);
        }
        "assert" => {
            let mut iter = args.iter();
            let condition = mbe::expect_fragment(
                db,
                &mut iter,
                parser::PrefixEntryPoint::Expr,
                args.top_subtree().delimiter.delim_span(),
            )
            .result()
            .map_err(|e| e.to_string())?;
            out.push(tt::Ident::new("if", span).into());
            out.open(DelimiterKind::Parenthesis, span);
            out.extend_with_tt(condition);
            out.close(span);
            out.open(DelimiterKind::Brace, span);
            out.close(span);
            if !iter.is_empty() {
                iter.expect_comma()
                    .map_err(|()| "expected a comma after assert condition")?;
                if !iter.is_empty() {
                    out.push(tt::Ident::new("else", span).into());
                    out.open(DelimiterKind::Brace, span);
                    out.push(tt::Ident::new("panic", span).into());
                    out.push(
                        tt::Punct {
                            char: '!',
                            spacing: tt::Spacing::Alone,
                            span,
                        }
                        .into(),
                    );
                    out.open(DelimiterKind::Parenthesis, span);
                    out.extend_with_tt(iter.remaining());
                    out.close(span);
                    out.close(span);
                }
            }
        }
        "include" => return Err("control flow depends on the included Rust source".into()),
        "cfg_select" => return Err("control flow depends on the active cfg branch".into()),
        _ => {
            return Err(
                format!("control flow of compiler builtin `{name}!` is unavailable").into(),
            );
        }
    }
    Ok(out.build())
}
