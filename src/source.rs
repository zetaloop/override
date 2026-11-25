use std::fmt;

use ra_ap_syntax::{
    AstNode, Edition, SyntaxElement, SyntaxNode, SyntaxToken, TextRange, TextSize, ast,
    syntax_editor::{SyntaxAnnotation, SyntaxEditor},
};

use crate::{Boundary, Result, Selector, flow, fragment, resolve::method::Target, symbol};

#[derive(Clone)]
pub struct Source {
    pub(crate) root: SyntaxNode,
    pub(crate) edition: Edition,
    pub(crate) module: String,
    pub(crate) modules: Vec<(String, SyntaxNode, Edition)>,
    methods: Vec<(SyntaxToken, Target)>,
}

impl Source {
    pub fn parse(text: &str, edition: Edition) -> Result<Self> {
        Ok(Self {
            root: fragment::file(text, edition)?,
            edition,
            module: String::new(),
            modules: Vec::new(),
            methods: Vec::new(),
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

    pub fn bind_method(&mut self, selector: Selector, target: &str) -> Result<()> {
        let at = self.locate(&selector)?;
        let name = ast::MethodCallExpr::cast(at.node.clone())
            .and_then(|call| call.name_ref())
            .ok_or("a method binding needs a selected method call")?;
        let target = Target::parse(target, name.text(), self.edition)?;
        let token = token_at(&self.root, at.at(name.syntax().clone()).range())
            .ok_or("method name has no source token")?;
        if let Some((_, binding)) = self.methods.iter_mut().find(|(t, _)| *t == token) {
            *binding = target;
        } else {
            self.methods.push((token, target));
        }
        Ok(())
    }

    pub(crate) fn bound(&self, at: &Location, name: TextRange) -> Option<&Target> {
        let root = at.parents.first().map_or(&at.root, |frame| &frame.root);
        if root != &self.root {
            return None;
        }
        let range = name + at.offset();
        self.methods
            .iter()
            .find_map(|(token, target)| (token.text_range() == range).then_some(target))
    }

    fn finish(
        &self,
        editor: SyntaxEditor,
        original: &SyntaxNode,
        offset: TextSize,
        inserted: impl IntoIterator<Item = (SyntaxToken, Target)>,
    ) -> (SyntaxNode, Vec<(SyntaxToken, Target)>) {
        let existing = self.methods.iter().filter_map(|(token, target)| {
            let range = token.text_range().checked_sub(offset)?;
            Some((token_at(original, range)?, target.clone()))
        });
        let annotations = existing
            .chain(inserted)
            .map(|(token, target)| {
                let mark = SyntaxAnnotation::default();
                editor.add_annotation(token, mark);
                (mark, target)
            })
            .collect::<Vec<_>>();
        let edit = editor.finish();
        let methods = annotations
            .into_iter()
            .flat_map(|(mark, target)| {
                edit.find_annotation(mark)
                    .iter()
                    .filter_map(SyntaxElement::as_token)
                    .cloned()
                    .map(move |token| (token, target.clone()))
            })
            .collect();
        (edit.new_root().clone(), methods)
    }

    pub(crate) fn roots(&self) -> impl Iterator<Item = Location> + '_ {
        std::iter::once((&self.module, &self.root, self.edition))
            .chain(self.modules.iter().map(|(m, r, e)| (m, r, *e)))
            .map(|(module, node, edition)| {
                let mut root = Location::root(node.clone(), edition);
                root.module = module.clone();
                root
            })
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
            position: None,
        })
    }

    pub fn declaration(&self, selector: Selector) -> Result<Declaration> {
        let location = self.locate(&selector)?;
        Ok(Declaration {
            location: if location.symbol.is_some() {
                *location.declaration.ok_or("symbol has no declaration")?
            } else {
                location
            },
        })
    }

    fn locate(&self, selector: &Selector) -> Result<Location> {
        let mut root = Location::root(self.root.clone(), self.edition);
        root.module = self.module.clone();
        let matches = selector.resolve(self, &[root])?;
        match matches.as_slice() {
            [location] => {
                let mut location = location.clone();
                if location.symbol.is_some() {
                    location.declaration = Some(Box::new(symbol::declaration(self, &location)?));
                }
                Ok(location)
            }
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

fn token_at(root: &SyntaxNode, range: TextRange) -> Option<SyntaxToken> {
    if !root.text_range().contains_range(range) {
        return None;
    }
    root.token_at_offset(range.start())
        .right_biased()
        .filter(|t| t.text_range() == range)
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
                krate: String::new(),
                module: String::new(),
                parents: Vec::new(),
                region: None,
                declaration: None,
                symbol: None,
                argument: false,
            },
        })
    }
}

pub struct Selected<'a> {
    pub(crate) source: &'a mut Source,
    pub(crate) location: Location,
    pub(crate) flow: flow::Options,
    pub(crate) position: Option<Boundary>,
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
            location: if self.location.symbol.is_some() {
                *self
                    .location
                    .declaration
                    .clone()
                    .expect("selected symbol has a declaration")
            } else {
                self.location.clone()
            },
        }
    }

    pub(crate) fn edit<T>(
        self,
        operation: impl FnOnce(&SyntaxEditor, &SyntaxNode, Edition) -> Result<T>,
    ) -> Result<T> {
        if self.location.region.is_some() {
            return Err("a region supports extraction and wrapping".into());
        }
        if self.location.symbol.is_some() {
            return Err("a symbol selection supports redirection".into());
        }
        if self.flow != flow::Options::default() {
            return Err("extraction options require extract()".into());
        }
        if self.position.is_some() {
            return Err("an insertion position requires adding a member".into());
        }
        let (editor, _) = SyntaxEditor::new(self.location.root.clone());
        let result = operation(&editor, &self.location.node, self.source.edition)?;
        let mut offset = self.location.offset();
        let (mut root, mut methods) = self.source.finish(editor, &self.location.root, offset, []);
        for parent in self.location.parents.iter().rev() {
            let text = root.to_string();
            let body = if parent.delimited {
                text.get(1..text.len().saturating_sub(1))
                    .ok_or("macro fragment has no delimiters")?
            } else {
                &text
            };
            let tree = fragment::token_tree(body, &parent.tree, self.source.edition)?;
            let delimiter = TextSize::from(u32::from(!parent.delimited));
            let start = tree.syntax().text_range().start() + delimiter;
            let inserted = methods.into_iter().filter_map(|(token, target)| {
                Some((token_at(tree.syntax(), token.text_range() + start)?, target))
            });
            offset -= parent.tree.syntax().text_range().start() + delimiter;
            let (editor, _) = SyntaxEditor::new(parent.root.clone());
            editor.replace(parent.tree.syntax(), tree.syntax());
            (root, methods) = self.source.finish(editor, &parent.root, offset, inserted);
        }
        self.source.root = root;
        self.source.methods = methods;
        Ok(result)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Location {
    pub root: SyntaxNode,
    pub node: SyntaxNode,
    pub edition: Edition,
    pub krate: String,
    pub module: String,
    pub parents: Vec<MacroFrame>,
    pub region: Option<Vec<SyntaxElement>>,
    pub declaration: Option<Box<Location>>,
    pub symbol: Option<String>,
    pub argument: bool,
}

impl PartialEq for Location {
    fn eq(&self, other: &Self) -> bool {
        self.symbol == other.symbol
            && self.argument == other.argument
            && self.region.is_some() == other.region.is_some()
            && self.krate == other.krate
            && self.module == other.module
            && self.same(other)
            && self.declaration == other.declaration
    }
}

impl Location {
    pub fn root(root: SyntaxNode, edition: Edition) -> Self {
        Self {
            node: root.clone(),
            edition,
            root,
            krate: String::new(),
            module: String::new(),
            parents: Vec::new(),
            region: None,
            declaration: None,
            symbol: None,
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
            symbol: (node == self.node).then(|| self.symbol.clone()).flatten(),
            argument: self.argument && node == self.node,
            node,
            root,
            parents,
            krate: self.krate.clone(),
            module: self.module.clone(),
            edition: self.edition,
        }
    }

    pub(crate) fn offset(&self) -> TextSize {
        self.parents
            .iter()
            .map(|frame| {
                frame.tree.syntax().text_range().start()
                    + TextSize::from(u32::from(!frame.delimited))
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

    pub fn contains(&self, other: &Self) -> bool {
        self.krate == other.krate
            && self.module == other.module
            && self.parents.first().map_or(&self.root, |f| &f.root)
                == other.parents.first().map_or(&other.root, |f| &f.root)
            && self.range().contains_range(other.range())
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
        for node in self.node.descendants() {
            nodes.push(self.at(node.clone()));
            if let Some(tree) = ast::MacroCall::cast(node).and_then(|call| call.token_tree())
                && let Some(input) = self.macro_input(tree)
            {
                input.visit(nodes);
            }
        }
    }

    pub fn find_at(&self, start: TextSize, matches: &impl Fn(&SyntaxNode) -> bool) -> Option<Self> {
        let byte = TextRange::at(start.checked_sub(self.offset())?, 1.into());
        let nodes = std::iter::successors(Some(self.node.clone()), |node| {
            node.child_or_token_at_range(byte)?.into_node()
        });
        for node in nodes {
            if node.text_range().start() == byte.start() && matches(&node) {
                return Some(self.at(node));
            }
            if let Some(tree) = ast::MacroCall::cast(node).and_then(|call| call.token_tree())
                && tree.syntax().text_range().contains_range(byte)
            {
                return self.macro_input(tree)?.find_at(start, matches);
            }
        }
        None
    }

    fn macro_input(&self, tree: ast::TokenTree) -> Option<Self> {
        let text = tree.to_string();
        let body = text.get(1..text.len().saturating_sub(1))?;
        let edition = self.edition;
        let (root, delimited) = fragment::file(body, edition)
            .map(|root| (root, false))
            .or_else(|_| {
                fragment::expression(&format!("{{{body}}}"), edition)
                    .or_else(|_| fragment::expression(&format!("({body})"), edition))
                    .map(|expression| (expression.syntax().clone(), true))
            })
            .ok()?;
        let mut input = Self::root(root, edition);
        input.krate = self.krate.clone();
        input.module = self.module.clone();
        input.parents = self.parents.clone();
        input.parents.push(MacroFrame {
            root: self.root.clone(),
            tree,
            delimited,
        });
        Some(input)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MacroFrame {
    pub root: SyntaxNode,
    pub tree: ast::TokenTree,
    pub delimited: bool,
}
