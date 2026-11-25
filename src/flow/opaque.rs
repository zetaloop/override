use ra_ap_syntax::{AstNode, SyntaxNode, ast};

use crate::{Result, Source, flow::Edit, source::Location};

#[derive(Default)]
pub(crate) struct Macros;

pub(crate) enum Scope {}

impl Scope {
    pub fn block_call(
        _: &Source,
        _: &Location,
        _: ra_ap_syntax::TextRange,
        name: &str,
        _: ra_ap_syntax::TextRange,
        calls: &[Location],
    ) -> Result<Option<(&'static str, ra_ap_syntax::TextRange)>> {
        let call = ast::MacroCall::cast(calls[0].node.clone()).unwrap();
        Err(format!("resolving block-local function `{name}` depends on `{}!` at {:?}; enable the `expand` feature",call.path().ok_or("macro call has no path")?,calls[0].range()).into())
    }
    pub fn new(_: &Source, _: &Location) -> Option<Self> {
        None
    }
    pub fn reference(
        &self,
        _: ra_ap_syntax::TextRange,
        _: &str,
        _: impl FnOnce(ast::UseTree, ra_ap_syntax::TextRange),
    ) -> bool {
        match *self {}
    }
}

impl Macros {
    pub fn source_range(
        &self,
        _: usize,
        _: ra_ap_syntax::TextRange,
    ) -> Option<ra_ap_syntax::TextRange> {
        None
    }
    pub fn bound<'s>(
        &self,
        source: &'s Source,
        _: usize,
        at: &Location,
        range: ra_ap_syntax::TextRange,
    ) -> Option<&'s crate::resolve::method::Target> {
        source.bound(at, range)
    }

    pub fn parents(&self, _: usize) -> impl Iterator<Item = &Location> + Clone {
        std::iter::empty()
    }

    pub fn expand(
        &mut self,
        _: &Source,
        _: usize,
        _: &Location,
        call: &ast::MacroCall,
        _: Option<&mut crate::flow::Imports>,
    ) -> Result<(usize, SyntaxNode)> {
        Err(format!(
            "control flow depends on `{}!` at {:?}; enable the `expand` feature",
            call.path().ok_or("macro call has no path")?,
            ra_ap_syntax::AstNode::syntax(call).text_range()
        )
        .into())
    }

    pub fn project(self, edits: Vec<Vec<Edit>>) -> Result<Vec<Edit>> {
        Ok(edits.into_iter().next().unwrap_or_default())
    }
}
