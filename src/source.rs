use std::fmt;

use ra_ap_syntax::{
    AstNode, Edition, SyntaxElement, SyntaxNode, TextRange, TextSize, ast,
    syntax_editor::SyntaxEditor,
};

use crate::{Result, Selector, flow, fragment};

#[derive(Clone)]
pub struct Source {
    pub(crate) root: SyntaxNode,
    pub(crate) edition: Edition,
    pub(crate) module: String,
    pub(crate) modules: Vec<(String, SyntaxNode, Edition)>,
}

impl Source {
    pub fn parse(text: &str, edition: Edition) -> Result<Self> {
        Ok(Self {
            root: fragment::file(text, edition)?,
            edition,
            module: String::new(),
            modules: Vec::new(),
        })
    }

    pub fn edition(&self) -> Edition {
        self.edition
    }

    pub fn set_module(&mut self, module: &str) -> Result<()> {
        self.module = if module.is_empty() {
            String::new()
        } else {
            crate::resolve::symbol(module, self.edition)?
        };
        Ok(())
    }

    pub fn add_source(&mut self, module: &str, source: &Source) {
        self.modules
            .push((module.to_owned(), source.root.clone(), source.edition));
    }

    pub fn describe(&mut self, description: &str) -> Result<()> {
        let root = match fragment::file(description, self.edition) {
            Ok(root) => root,
            Err(declaration_error) => {
                let root = fragment::file(&format!("fn f() {{ let {description} = (); }}"), self.edition)
                    .map_err(|pattern_error| format!("invalid interface declaration ({declaration_error}) or binding pattern ({pattern_error})"))?;
                let binding = fragment::one::<ast::LetStmt>(&root)?;
                let pattern = binding.pat().ok_or("description has no binding pattern")?;
                if !matches!(
                    pattern,
                    ast::Pat::TupleStructPat(_) | ast::Pat::RecordPat(_)
                ) {
                    return Err("a binding description must name its type or variant".into());
                }
                pattern.syntax().clone_subtree()
            }
        };
        self.modules.push((self.module.clone(), root, self.edition));
        Ok(())
    }

    pub fn select(&mut self, selector: Selector) -> Result<Selected<'_>> {
        let location = self.locate(&selector)?;
        Ok(Selected {
            source: self,
            location,
            flow: flow::Options::default(),
        })
    }

    pub fn declaration(&self, selector: Selector) -> Result<Declaration> {
        Ok(Declaration {
            location: self.locate(&selector)?,
        })
    }

    fn locate(&self, selector: &Selector) -> Result<Location> {
        let mut root = Location::root(self.root.clone(), self.edition);
        root.module = self.module.clone();
        let matches = selector.resolve(self, &[root])?;
        match matches.as_slice() {
            [location] => Ok(location.clone()),
            [] => Err(format!("no target matches {selector:?}").into()),
            _ => Err(format!(
                "ambiguous target {selector:?}: {}",
                matches
                    .iter()
                    .map(|location| format!(
                        "{} at {:?}",
                        crate::resolve::qualified(location),
                        location.range()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .into()),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.root, formatter)
    }
}

#[derive(Clone, Debug)]
pub struct Declaration {
    pub(crate) location: Location,
}

impl Declaration {
    pub fn parse(signature: &str, edition: Edition) -> Result<Self> {
        let function = fragment::signature(signature, edition)?;
        let root = function
            .syntax()
            .ancestors()
            .last()
            .ok_or("missing declaration root")?;
        Ok(Self {
            location: Location {
                root,
                node: function.syntax().clone(),
                edition,
                module: String::new(),
                parents: Vec::new(),
                region: None,
                declaration: None,
                argument: false,
            },
        })
    }
}

pub struct Selected<'a> {
    pub(crate) source: &'a mut Source,
    pub(crate) location: Location,
    pub(crate) flow: flow::Options,
}

impl Selected<'_> {
    pub fn text(&self) -> String {
        self.location.region.as_ref().map_or_else(
            || self.location.node.to_string(),
            |region| region.iter().map(ToString::to_string).collect(),
        )
    }

    pub fn declaration(&self) -> Declaration {
        Declaration {
            location: self.location.clone(),
        }
    }

    pub(crate) fn edit<T>(
        self,
        operation: impl FnOnce(&SyntaxEditor, &SyntaxNode, Edition) -> Result<T>,
    ) -> Result<T> {
        if self.location.region.is_some() {
            return Err("a region supports extraction and delegation".into());
        }
        if self.flow != flow::Options::default() {
            return Err("control-flow options require extraction".into());
        }
        let (editor, _) = SyntaxEditor::new(self.location.root.clone());
        let result = operation(&editor, &self.location.node, self.source.edition)?;
        let mut root = editor.finish().new_root().clone();
        for parent in self.location.parents.iter().rev() {
            let text = root.to_string();
            let body = if parent.block {
                text.strip_prefix('{')
                    .and_then(|text| text.strip_suffix('}'))
                    .ok_or("macro block has no delimiters")?
            } else {
                &text
            };
            let tree = fragment::token_tree(body, &parent.tree, self.source.edition)?;
            let (editor, _) = SyntaxEditor::new(parent.root.clone());
            editor.replace(parent.tree.syntax(), tree.syntax());
            root = editor.finish().new_root().clone();
        }
        self.source.root = root;
        Ok(result)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Location {
    pub root: SyntaxNode,
    pub node: SyntaxNode,
    pub edition: Edition,
    pub module: String,
    pub parents: Vec<MacroFrame>,
    pub region: Option<Vec<SyntaxElement>>,
    pub declaration: Option<Box<Location>>,
    pub argument: bool,
}

impl Location {
    pub fn root(root: SyntaxNode, edition: Edition) -> Self {
        Self {
            node: root.clone(),
            edition,
            root,
            module: String::new(),
            parents: Vec::new(),
            region: None,
            declaration: None,
            argument: false,
        }
    }

    pub fn at(&self, node: SyntaxNode) -> Self {
        let root = node.ancestors().last().unwrap_or_else(|| node.clone());
        let parents = if root == self.root {
            self.parents.clone()
        } else {
            self.parents
                .iter()
                .position(|frame| frame.root == root)
                .map_or_else(Vec::new, |index| self.parents[..index].to_vec())
        };
        Self {
            region: (node == self.node).then(|| self.region.clone()).flatten(),
            declaration: (node == self.node)
                .then(|| self.declaration.clone())
                .flatten(),
            argument: self.argument && node == self.node,
            node,
            root,
            parents,
            module: self.module.clone(),
            edition: self.edition,
        }
    }

    fn offset(&self) -> TextSize {
        self.parents
            .iter()
            .map(|frame| {
                frame.tree.syntax().text_range().start() + TextSize::from(u32::from(!frame.block))
            })
            .sum()
    }

    pub fn range(&self) -> TextRange {
        let range = self
            .region
            .as_ref()
            .and_then(|region| {
                Some(
                    region
                        .first()?
                        .text_range()
                        .cover(region.last()?.text_range()),
                )
            })
            .unwrap_or_else(|| self.node.text_range());
        let offset = self.offset();
        TextRange::new(range.start() + offset, range.end() + offset)
    }

    pub fn same(&self, other: &Self) -> bool {
        self.node.kind() == other.node.kind()
            && self.range() == other.range()
            && self.parents.first().map_or(&self.root, |frame| &frame.root)
                == other
                    .parents
                    .first()
                    .map_or(&other.root, |frame| &frame.root)
            && self
                .parents
                .iter()
                .map(|frame| frame.tree.syntax().text_range())
                .eq(other
                    .parents
                    .iter()
                    .map(|frame| frame.tree.syntax().text_range()))
    }

    pub fn ancestors(&self) -> Vec<SyntaxNode> {
        let mut nodes: Vec<_> = self.node.ancestors().collect();
        for frame in self.parents.iter().rev() {
            nodes.extend(frame.tree.syntax().ancestors().skip(1));
        }
        nodes
    }

    pub fn descendants(&self) -> Vec<Self> {
        let mut nodes = Vec::new();
        self.visit(&mut nodes);
        if let Some(region) = &self.region
            && let (Some(first), Some(last)) = (region.first(), region.last())
        {
            let offset = self.offset();
            let range = TextRange::new(
                first.text_range().start() + offset,
                last.text_range().end() + offset,
            );
            nodes.retain(|node| range.contains_range(node.range()));
        }
        nodes
    }

    fn visit(&self, nodes: &mut Vec<Self>) {
        let edition = self.edition;
        for node in self.node.descendants() {
            nodes.push(self.at(node.clone()));
            let Some(tree) = ast::MacroCall::cast(node).and_then(|call| call.token_tree()) else {
                continue;
            };
            let text = tree.to_string();
            let Some(body) = text.get(1..text.len().saturating_sub(1)) else {
                continue;
            };
            let parsed = fragment::file(body, edition)
                .map(|root| (root, false))
                .or_else(|_| {
                    fragment::expression(&format!("{{{body}}}"), edition)
                        .map(|block| (block.syntax().clone(), true))
                });
            let Ok((root, block)) = parsed else { continue };
            let mut parents = self.parents.clone();
            parents.push(MacroFrame {
                root: self.root.clone(),
                tree,
                block,
            });
            Self {
                root: root.clone(),
                node: root,
                edition,
                module: self.module.clone(),
                parents,
                region: None,
                declaration: None,
                argument: false,
            }
            .visit(nodes);
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MacroFrame {
    pub root: SyntaxNode,
    pub tree: ast::TokenTree,
    pub block: bool,
}
