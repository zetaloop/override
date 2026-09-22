use ra_ap_syntax::{AstNode, SyntaxKind, SyntaxNode, T, TextRange, TextSize, ast};

use crate::{Result, Selector, Source, select, source::Location};

#[derive(Clone, Debug)]
pub struct Boundary {
    selector: Selector,
    edge: Edge,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Edge {
    Before,
    After,
    Start,
    End,
}

impl Boundary {
    pub(crate) fn new(selector: Selector, edge: Edge) -> Self {
        Self { selector, edge }
    }

    fn resolve(&self, source: &Source, scope: &Location) -> Result<(Location, TextSize)> {
        let matches = self.selector.resolve(source, std::slice::from_ref(scope))?;
        let [location] = matches.as_slice() else {
            return Err(
                format!("region boundary {self:?} matches {} objects", matches.len()).into(),
            );
        };
        let after = matches!(self.edge, Edge::After | Edge::End);
        if let Some(region) = &location.region {
            let first = region.first().ok_or("selected region is empty")?;
            let last = region.last().ok_or("selected region is empty")?;
            let offset = if after {
                last.text_range().end()
            } else {
                first.text_range().start()
            };
            return Ok(point(location, location.node.clone(), offset));
        }
        let mut node = location.node.clone();
        if matches!(self.edge, Edge::Start | Edge::End) {
            let body = select::body(&location.node).unwrap_or_else(|| location.node.clone());
            let list = body
                .children()
                .find(|node| {
                    matches!(
                        node.kind(),
                        SyntaxKind::STMT_LIST | SyntaxKind::ITEM_LIST | SyntaxKind::ASSOC_ITEM_LIST
                    )
                })
                .unwrap_or_else(|| body.clone());
            if matches!(
                list.kind(),
                SyntaxKind::STMT_LIST
                    | SyntaxKind::ITEM_LIST
                    | SyntaxKind::ASSOC_ITEM_LIST
                    | SyntaxKind::SOURCE_FILE
            ) {
                let offset = if after {
                    list.last_child_or_token()
                        .and_then(|element| element.into_token())
                        .filter(|token| token.kind() == T!['}'])
                        .map_or(list.text_range().end(), |token| token.text_range().start())
                } else {
                    list.first_child_or_token()
                        .and_then(|element| element.into_token())
                        .filter(|token| token.kind() == T!['{'])
                        .map_or(list.text_range().start(), |token| token.text_range().end())
                };
                return Ok(point(location, list, offset));
            }
            node = body;
        }
        if ast::IdentPat::can_cast(node.kind()) {
            node = node
                .ancestors()
                .find(|node| {
                    ast::Stmt::can_cast(node.kind())
                        || ast::Expr::can_cast(node.kind())
                        || matches!(node.kind(), SyntaxKind::PARAM | SyntaxKind::MATCH_ARM)
                })
                .ok_or("binding has no enclosing declaration")?;
        }
        if let Some(statement) = node
            .parent()
            .filter(|node| ast::ExprStmt::can_cast(node.kind()))
        {
            node = statement;
        }
        Ok(edge(location, node, after))
    }
}

fn edge(location: &Location, node: SyntaxNode, after: bool) -> (Location, TextSize) {
    let offset = if after {
        node.text_range().end()
    } else {
        node.text_range().start()
    };
    point(location, node.parent().unwrap_or(node), offset)
}

fn point(location: &Location, container: SyntaxNode, offset: TextSize) -> (Location, TextSize) {
    let mut location = location.at(container);
    location.region = None;
    let offset = offset + location.range().start() - location.node.text_range().start();
    (location, offset)
}

pub(crate) fn select(
    source: &Source,
    scope: &Location,
    start: &Boundary,
    end: &Boundary,
) -> Result<Location> {
    let (start, from) = start.resolve(source, scope)?;
    let (end, to) = end.resolve(source, scope)?;
    if from >= to {
        return Err("region end must follow its start".into());
    }
    let container = start
        .ancestors()
        .into_iter()
        .map(|node| start.at(node))
        .find(|location| {
            end.ancestors()
                .into_iter()
                .any(|node| location.same(&end.at(node)))
        })
        .ok_or("region boundaries have no common syntax container")?;
    let offset = container.range().start() - container.node.text_range().start();
    let range = TextRange::new(from - offset, to - offset);
    let mut region = Vec::new();
    for element in container.node.children_with_tokens() {
        if range.contains_range(element.text_range()) {
            region.push(element);
        } else if element
            .text_range()
            .intersect(range)
            .is_some_and(|overlap| !overlap.is_empty())
        {
            return Err("region boundary splits a syntax element".into());
        }
    }
    if !region.iter().any(|element| element.as_node().is_some()) {
        return Err("selected region has no syntax nodes".into());
    }
    Ok(Location {
        region: Some(region),
        ..container
    })
}

pub(crate) fn tail(container: &SyntaxNode, region: &[ra_ap_syntax::SyntaxElement]) -> bool {
    let Some(last) = region.iter().rfind(|element| !element.kind().is_trivia()) else {
        return false;
    };
    if ast::ClosureExpr::can_cast(container.kind()) {
        return select::body(container).as_ref() == last.as_node();
    }
    let Some(list) = ast::StmtList::cast(container.clone()) else {
        return false;
    };
    let final_element = list
        .syntax()
        .children_with_tokens()
        .filter(|element| !element.kind().is_trivia() && element.kind() != T!['}'])
        .last();
    if final_element.as_ref() != Some(last) {
        return false;
    }
    let Some(block) = container.parent() else {
        return false;
    };
    block.parent().is_some_and(|owner| {
        matches!(owner.kind(), SyntaxKind::FN | SyntaxKind::CLOSURE_EXPR)
            && select::body(&owner).as_ref() == Some(&block)
    })
}

pub(crate) fn expression(container: &SyntaxNode, region: &[ra_ap_syntax::SyntaxElement]) -> bool {
    tail(container, region)
        || region
            .iter()
            .rfind(|element| !element.kind().is_trivia())
            .is_some_and(|element| ast::Expr::can_cast(element.kind()))
}
