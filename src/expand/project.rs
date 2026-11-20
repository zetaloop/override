use ra_ap_syntax::{
    AstNode, Edition, SyntaxKind, SyntaxNode, SyntaxToken, SyntaxTreeBuilder, TextRange, TextSize,
};
use span::{EditionedFileId, FileId, Span};
use syntax_bridge::{DocCommentDesugarMode, SpanMapper};

use super::{GENERATED, INPUT, Macros, Origin, View, tokens, transcribe};
use crate::{Result, flow::Edit};

const INSERTED: u32 = 3;

enum From {
    Token(TextSize),
    Inserted(Span),
}

struct Ranges<'a> {
    original: &'a dyn SpanMapper,
    ends: Vec<(TextSize, From)>,
}

impl SpanMapper for Ranges<'_> {
    fn span_for(&self, range: TextRange) -> Span {
        let i = self.ends.partition_point(|(end, _)| *end <= range.start());
        match self.ends[i].1 {
            From::Inserted(span) => span,
            From::Token(old) => {
                let start = if i == 0 { 0.into() } else { self.ends[i - 1].0 };
                self.original.span_for((range - start) + old)
            }
        }
    }
}

fn edited_tree(
    node: &SyntaxNode,
    edits: &[Edit],
    origins: &dyn SpanMapper,
    site: Span,
    edition: Edition,
) -> tt::TopSubtree {
    let mut tree = SyntaxTreeBuilder::default();
    tree.start_node(SyntaxKind::TOKEN_TREE);
    let mut ends = Vec::new();
    let mut end = TextSize::new(0);
    let mut emit = |kind, text: &str, origin| {
        tree.token(kind, text);
        end += TextSize::of(text);
        ends.push((end, origin));
    };
    for token in tokens(node) {
        let start = token.text_range().start();
        if let Some(edit) = edits.iter().find(|e| e.token == token) {
            let mut span = origins.span_for(token.text_range());
            span.anchor.file_id =
                EditionedFileId::new(FileId::from_raw(INSERTED), span.anchor.file_id.edition());
            for (part, original) in [
                (&edit.before, false),
                (&edit.value, edit.value == token.text()),
                (&edit.after, false),
            ] {
                if original {
                    emit(token.kind(), token.text(), From::Token(start));
                } else {
                    let lexed = parser::LexedStr::new(edition, part);
                    for i in 0..lexed.len() {
                        emit(lexed.kind(i), lexed.text(i), From::Inserted(span));
                    }
                }
            }
        } else {
            emit(token.kind(), token.text(), From::Token(start));
        }
    }
    tree.finish_node();
    syntax_bridge::syntax_node_to_token_tree(
        &tree.finish().syntax_node(),
        Ranges {
            original: origins,
            ends,
        },
        site,
        DocCommentDesugarMode::Mbe,
    )
}

impl View {
    fn input_token(&self, token: &SyntaxToken) -> Result<SyntaxToken> {
        let span = self.expansion.map.span_at(token.text_range().start());
        if span.anchor.file_id.file_id().index() != INPUT {
            return Err(format!(
                "`{}` comes from the macro definition or builtin, not the call arguments",
                token.text()
            )
            .into());
        }
        self.input
            .syntax()
            .token_at_offset(span.range.start())
            .right_biased()
            .filter(|t| t.text_range() == span.range && t.text() == token.text())
            .ok_or_else(|| {
                format!(
                    "`{}` does not correspond to one original input token",
                    token.text()
                )
                .into()
            })
    }
}

impl Macros {
    pub fn project(self, mut edits: Vec<Vec<Edit>>) -> Result<Vec<Edit>> {
        while edits.len() > 1 {
            let i = edits.len() - 1;
            let changes = edits.pop().unwrap();
            if changes.is_empty() {
                continue;
            }
            let v = &self.views[i - 1];
            let name = v.site.node.to_string();
            let mut input = Vec::<Edit>::new();
            for e in &changes {
                let token = v
                    .input_token(&e.token)
                    .map_err(|err| format!("cannot rewrite `{name}`: {err}"))?;
                let projected = Edit { token, ..e.clone() };
                if let Some(existing) = input.iter().find(|e| e.token == projected.token) {
                    if *existing != projected {
                        return Err(format!(
                            "`{name}` needs different edits to the same input token"
                        )
                        .into());
                    }
                } else {
                    input.push(projected);
                }
            }
            let parent = v
                .parent
                .checked_sub(1)
                .map(|p| &self.views[p].expansion.map);
            let origin = Origin {
                file: INPUT,
                edition: v.site.edition,
                parent,
            };
            let path = ra_ap_syntax::ast::MacroCall::cast(v.site.node.clone())
                .and_then(|c| c.path())
                .ok_or("macro call has no path")?;
            let edition = origin
                .span_for(path.syntax().text_range())
                .ctx
                .edition(&self.db);
            let site = Origin {
                file: GENERATED,
                edition,
                parent: None,
            }
            .span_for(path.syntax().text_range());
            let args = edited_tree(v.input.syntax(), &input, &origin, site, edition);
            let actual = transcribe(&self.db, &v.rule, &args, &v.site.node, site)
                .map_err(|e| format!("rewriting `{name}` changes its expansion: {e}"))?;
            if actual.arm != v.expansion.arm {
                return Err(format!("rewriting `{name}` selects a different macro rule").into());
            }
            let expected =
                edited_tree(&v.expansion.root, &changes, &v.expansion.map, site, edition);
            let result = syntax_bridge::syntax_node_to_token_tree(
                &actual.root,
                &actual.map,
                site,
                DocCommentDesugarMode::Mbe,
            );
            if !result
                .as_token_trees()
                .iter_flat_tokens()
                .eq(expected.as_token_trees().iter_flat_tokens())
            {
                return Err(format!(
                    "rewriting `{name}` would also change other tokens in its expansion"
                )
                .into());
            }
            edits[v.parent].extend(input);
        }
        Ok(edits.pop().unwrap_or_default())
    }
}
