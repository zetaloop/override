use ra_ap_syntax::{
    AstNode, SyntaxKind, ast,
    ast::{HasArgList, HasLoopBody, HasName},
};

use crate::{
    Declaration, Result, Source, fragment,
    region::{self, Boundary, Edge},
    resolve,
    source::Location,
};

#[derive(Clone, Debug, Default)]
pub struct Selector {
    steps: Vec<Step>,
}

#[derive(Clone, Debug)]
enum Step {
    Find(Matcher),
    Field(String),
    Parameter(String),
    Argument(String),
    Body,
    Condition,
    Region(Boundary, Boundary),
    Has(Selector),
    And(Selector),
    Not(Selector),
    Or(Selector, Selector),
    Child(Selector),
    Trait(String),
    Type(String),
    References(String),
    Declaration(Declaration),
}

#[derive(Clone, Debug)]
enum Matcher {
    Item(String),
    Import(String),
    Implementation(String),
    Call(String),
    Record(String),
    Pattern(String),
    Arm(String),
    Binding(String),
    Macro(String),
    Attribute(String),
    Variant(String),
    Generic(String),
    Kind(SyntaxKind),
}

pub fn root() -> Selector {
    Selector::default()
}
pub fn item(name: &str) -> Selector {
    root().item(name)
}
pub fn call(name: &str) -> Selector {
    root().call(name)
}
pub fn arm(variant: &str) -> Selector {
    root().arm(variant)
}

impl Selector {
    fn step(mut self, step: Step) -> Self {
        self.steps.push(step);
        self
    }
    fn find(self, matcher: Matcher) -> Self {
        self.step(Step::Find(matcher))
    }

    pub fn item(self, name: &str) -> Self {
        self.find(Matcher::Item(name.to_owned()))
    }
    pub fn call(self, name: &str) -> Self {
        self.find(Matcher::Call(name.to_owned()))
    }
    pub fn import(self, name: &str) -> Self {
        self.find(Matcher::Import(name.to_owned()))
    }
    pub fn implementation(self, ty: &str) -> Self {
        self.find(Matcher::Implementation(ty.to_owned()))
    }
    pub fn record(self, name: &str) -> Self {
        self.find(Matcher::Record(name.to_owned()))
    }
    pub fn pattern(self, name: &str) -> Self {
        self.find(Matcher::Pattern(name.to_owned()))
    }
    pub fn arm(self, variant: &str) -> Self {
        self.find(Matcher::Arm(variant.to_owned()))
    }
    pub fn binding(self, name: &str) -> Self {
        self.find(Matcher::Binding(name.to_owned()))
    }
    pub fn macro_call(self, name: &str) -> Self {
        self.find(Matcher::Macro(name.to_owned()))
    }
    pub fn attribute(self, name: &str) -> Self {
        self.find(Matcher::Attribute(name.to_owned()))
    }
    pub fn variant(self, name: &str) -> Self {
        self.find(Matcher::Variant(name.to_owned()))
    }
    pub fn generic(self, name: &str) -> Self {
        self.find(Matcher::Generic(name.to_owned()))
    }
    pub fn for_loop(self) -> Self {
        self.find(Matcher::Kind(SyntaxKind::FOR_EXPR))
    }
    pub fn while_loop(self) -> Self {
        self.find(Matcher::Kind(SyntaxKind::WHILE_EXPR))
    }
    pub fn loop_expr(self) -> Self {
        self.find(Matcher::Kind(SyntaxKind::LOOP_EXPR))
    }
    pub fn if_expr(self) -> Self {
        self.find(Matcher::Kind(SyntaxKind::IF_EXPR))
    }
    pub fn match_expr(self) -> Self {
        self.find(Matcher::Kind(SyntaxKind::MATCH_EXPR))
    }
    pub fn closure(self) -> Self {
        self.find(Matcher::Kind(SyntaxKind::CLOSURE_EXPR))
    }
    pub fn field(self, name: &str) -> Self {
        self.step(Step::Field(name.to_owned()))
    }
    pub fn parameter(self, name: &str) -> Self {
        self.step(Step::Parameter(name.to_owned()))
    }
    pub fn argument(self, name: &str) -> Self {
        self.step(Step::Argument(name.to_owned()))
    }
    pub fn body(self) -> Self {
        self.step(Step::Body)
    }
    pub fn condition(self) -> Self {
        self.step(Step::Condition)
    }
    pub fn region(self, start: Boundary, end: Boundary) -> Self {
        self.step(Step::Region(start, end))
    }
    pub fn before(self) -> Boundary {
        Boundary::new(self, Edge::Before)
    }
    pub fn after(self) -> Boundary {
        Boundary::new(self, Edge::After)
    }
    pub fn start(self) -> Boundary {
        Boundary::new(self, Edge::Start)
    }
    pub fn end(self) -> Boundary {
        Boundary::new(self, Edge::End)
    }
    pub fn has(self, query: Selector) -> Self {
        self.step(Step::Has(query))
    }
    pub fn and(self, query: Selector) -> Self {
        self.step(Step::And(query))
    }
    pub fn not(self, query: Selector) -> Self {
        self.step(Step::Not(query))
    }
    pub fn or(self, query: Selector) -> Self {
        root().step(Step::Or(self, query))
    }
    pub fn child(self, query: Selector) -> Self {
        self.step(Step::Child(query))
    }
    pub fn implementing(self, name: &str) -> Self {
        self.step(Step::Trait(name.to_owned()))
    }
    pub fn of_type(self, ty: &str) -> Self {
        self.step(Step::Type(ty.to_owned()))
    }
    pub fn references(self, name: &str) -> Self {
        self.step(Step::References(name.to_owned()))
    }
    pub fn declared_by(self, declaration: &Declaration) -> Self {
        self.step(Step::Declaration(declaration.clone()))
    }

    pub(crate) fn resolve(&self, source: &Source, scopes: &[Location]) -> Result<Vec<Location>> {
        let mut current = scopes.to_vec();
        for step in &self.steps {
            let mut next = Vec::new();
            for scope in &current {
                match step {
                    Step::Find(matcher) => {
                        let matcher = matcher.prepare(source.edition)?;
                        for candidate in scope.descendants() {
                            if matcher.matches(&candidate) {
                                next.push(candidate);
                            }
                        }
                    }
                    Step::Field(name) => next.extend(resolve::fields(source, scope, name)?),
                    Step::Parameter(name) => {
                        if scope.region.is_some() {
                            return Err("a region has no parameter list".into());
                        }
                        if name != "self" {
                            fragment::name(name, source.edition)?;
                        }
                        let list = scope
                            .node
                            .children()
                            .find_map(ast::ParamList::cast)
                            .ok_or("selected object has no parameters")?;
                        for parameter in list.params() {
                            if parameter.pat().is_some_and(|pattern| {
                                resolve::pattern_names(&pattern)
                                    .iter()
                                    .any(|binding| binding == name)
                            }) {
                                next.push(scope.at(parameter.syntax().clone()));
                            }
                        }
                        if name == "self"
                            && let Some(receiver) = list.self_param()
                        {
                            next.push(scope.at(receiver.syntax().clone()));
                        }
                    }
                    Step::Argument(name) => {
                        let mut argument = resolve::argument(source, scope, name)?;
                        argument.argument = name != "self";
                        next.push(argument);
                    }
                    Step::Body => {
                        if scope.region.is_some() {
                            return Err("a region is already a body selection".into());
                        }
                        next.push(
                            scope.at(body(&scope.node).ok_or("selected object has no body")?),
                        );
                    }
                    Step::Condition => {
                        let condition = ast::IfExpr::cast(scope.node.clone())
                            .and_then(|node| node.condition())
                            .or_else(|| {
                                ast::WhileExpr::cast(scope.node.clone())
                                    .and_then(|node| node.condition())
                            })
                            .or_else(|| {
                                ast::ForExpr::cast(scope.node.clone())
                                    .and_then(|node| node.iterable())
                            });
                        next.push(
                            scope.at(condition
                                .ok_or("selected object has no condition")?
                                .syntax()
                                .clone()),
                        );
                    }
                    Step::Region(start, end) => {
                        next.push(region::select(source, scope, start, end)?);
                    }
                    Step::Has(query) => {
                        if !query
                            .resolve(source, std::slice::from_ref(scope))?
                            .is_empty()
                        {
                            next.push(scope.clone());
                        }
                    }
                    Step::And(query) | Step::Not(query) => {
                        let matches = query
                            .resolve(source, std::slice::from_ref(scope))?
                            .iter()
                            .any(|node| node.same(scope));
                        if matches != matches!(step, Step::Not(_)) {
                            next.push(scope.clone());
                        }
                    }
                    Step::Or(left, right) => {
                        next.extend(left.resolve(source, std::slice::from_ref(scope))?);
                        next.extend(right.resolve(source, std::slice::from_ref(scope))?);
                    }
                    Step::Child(query) => {
                        next.extend(
                            query
                                .resolve(source, std::slice::from_ref(scope))?
                                .into_iter()
                                .filter(|node| {
                                    node.ancestors()
                                        .into_iter()
                                        .skip(1)
                                        .find(|ancestor| {
                                            ancestor == &scope.node
                                                || (resolve::is_object(ancestor.kind())
                                                    && !ast::MacroCall::can_cast(ancestor.kind())
                                                    && scope.range().contains_range(
                                                        node.at(ancestor.clone()).range(),
                                                    ))
                                        })
                                        .is_some_and(|parent| parent == scope.node)
                                }),
                        );
                    }
                    Step::Trait(name) => {
                        let expected =
                            fragment::spelling(fragment::ty(name, source.edition)?.syntax());
                        if scope
                            .ancestors()
                            .into_iter()
                            .filter_map(ast::Impl::cast)
                            .filter_map(|implementation| implementation.trait_())
                            .any(|ty| fragment::spelling(ty.syntax()) == expected)
                        {
                            next.push(scope.clone());
                        }
                    }
                    Step::Type(text) => {
                        let expected = fragment::ty(text, source.edition)?;
                        if resolve::node_type(&scope.node)
                            .is_some_and(|ty| resolve::types_equal(source, scope, &ty, &expected))
                        {
                            next.push(scope.clone());
                        }
                    }
                    Step::References(text) => {
                        let expected = fragment::expression(text, source.edition)?;
                        if !matches!(expected, ast::Expr::PathExpr(_) | ast::Expr::FieldExpr(_)) {
                            return Err("a reference query requires a named path or field".into());
                        }
                        let expected = fragment::spelling(expected.syntax());
                        if scope.descendants().iter().any(|location| {
                            matches!(
                                location.node.kind(),
                                SyntaxKind::PATH_EXPR | SyntaxKind::FIELD_EXPR
                            ) && fragment::spelling(&location.node) == expected
                        }) {
                            next.push(scope.clone());
                        }
                    }
                    Step::Declaration(declaration) => {
                        if !matches!(
                            scope.node.kind(),
                            SyntaxKind::CALL_EXPR | SyntaxKind::METHOD_CALL_EXPR
                        ) {
                            return Err("declaration association requires a call".into());
                        }
                        let mut location = scope.clone();
                        location.declaration = Some(Box::new(declaration.location.clone()));
                        next.push(location);
                    }
                }
            }
            current.clear();
            for candidate in next {
                if !current.iter().any(|existing| candidate.same(existing)) {
                    current.push(candidate);
                }
            }
        }
        Ok(current)
    }
}

impl Matcher {
    fn prepare(&self, edition: ra_ap_syntax::Edition) -> Result<Self> {
        let mut matcher = self.clone();
        match &mut matcher {
            Self::Item(name)
            | Self::Import(name)
            | Self::Call(name)
            | Self::Record(name)
            | Self::Pattern(name)
            | Self::Macro(name)
            | Self::Attribute(name) => *name = resolve::symbol(name, edition)?,
            Self::Implementation(ty) => {
                *ty = fragment::spelling(fragment::ty(ty, edition)?.syntax())
            }
            Self::Arm(pattern) => {
                let parsed =
                    fragment::expression(&format!("match () {{ {pattern} => () }}"), edition)?;
                let arm = fragment::one::<ast::MatchArm>(parsed.syntax())?;
                if arm.guard().is_some() {
                    return Err("an arm query requires a pattern".into());
                }
                *pattern = fragment::spelling(arm.pat().ok_or("arm has no pattern")?.syntax());
            }
            Self::Binding(name) | Self::Variant(name) => {
                *name = fragment::name(name, edition)?.text().to_string()
            }
            Self::Generic(name) => {
                fragment::signature(&format!("fn f<{name}>()"), edition)?;
            }
            Self::Kind(_) => {}
        }
        Ok(matcher)
    }

    fn matches(&self, location: &Location) -> bool {
        let node = &location.node;
        match self {
            Self::Kind(kind) => node.kind() == *kind,
            Self::Item(expected) => {
                ast::Item::can_cast(node.kind())
                    && node
                        .children()
                        .any(|child| ast::Name::can_cast(child.kind()))
                    && resolve::matches_name(&resolve::qualified(location), expected)
            }
            Self::Implementation(expected) => ast::Impl::cast(node.clone())
                .and_then(|implementation| implementation.self_ty())
                .is_some_and(|ty| fragment::spelling(ty.syntax()) == *expected),
            Self::Call(expected) => {
                resolve::call_name(node).is_some_and(|name| resolve::matches_name(&name, expected))
            }
            Self::Import(expected) => ast::UseTree::cast(node.clone()).is_some_and(|tree| {
                resolve::matches_name(&resolve::import_path(&tree), expected)
                    || tree
                        .rename()
                        .and_then(|rename| rename.name())
                        .is_some_and(|name| name.text() == expected)
            }),
            Self::Record(expected) => ast::RecordExpr::cast(node.clone())
                .and_then(|record| record.path())
                .is_some_and(|path| resolve::matches_path(&path, expected)),
            Self::Pattern(expected) => ast::RecordPat::cast(node.clone())
                .and_then(|pattern| pattern.path())
                .or_else(|| {
                    ast::TupleStructPat::cast(node.clone()).and_then(|pattern| pattern.path())
                })
                .or_else(|| ast::PathPat::cast(node.clone()).and_then(|pattern| pattern.path()))
                .is_some_and(|path| resolve::matches_path(&path, expected)),
            Self::Arm(expected) => ast::MatchArm::cast(node.clone())
                .and_then(|arm| arm.pat())
                .is_some_and(|pattern| {
                    fragment::spelling(pattern.syntax()) == *expected
                        || pattern
                            .syntax()
                            .descendants()
                            .filter_map(ast::Path::cast)
                            .any(|path| resolve::matches_path(&path, expected))
                }),
            Self::Binding(name) => ast::IdentPat::cast(node.clone())
                .and_then(|binding| binding.name())
                .is_some_and(|binding| binding.text() == name),
            Self::Macro(expected) => ast::MacroCall::cast(node.clone())
                .and_then(|call| call.path())
                .is_some_and(|path| resolve::matches_path(&path, expected)),
            Self::Attribute(expected) => {
                ast::AnyAttr::cast(node.clone()).is_some_and(|attribute| {
                    attribute
                        .syntax()
                        .descendants()
                        .find_map(ast::Path::cast)
                        .is_some_and(|path| resolve::matches_path(&path, expected))
                })
            }
            Self::Variant(name) => ast::Variant::cast(node.clone())
                .and_then(|variant| variant.name())
                .is_some_and(|variant| variant.text() == name),
            Self::Generic(name) => {
                matches!(
                    node.kind(),
                    SyntaxKind::TYPE_PARAM | SyntaxKind::CONST_PARAM | SyntaxKind::LIFETIME_PARAM
                ) && node.children().any(|child| {
                    matches!(child.kind(), SyntaxKind::NAME | SyntaxKind::LIFETIME)
                        && child.to_string() == *name
                })
            }
        }
    }
}

pub(crate) fn body(node: &ra_ap_syntax::SyntaxNode) -> Option<ra_ap_syntax::SyntaxNode> {
    if let Some(function) = ast::Fn::cast(node.clone()) {
        function.body().map(|body| body.syntax().clone())
    } else if let Some(for_loop) = ast::ForExpr::cast(node.clone()) {
        for_loop.loop_body().map(|body| body.syntax().clone())
    } else if let Some(while_loop) = ast::WhileExpr::cast(node.clone()) {
        while_loop.loop_body().map(|body| body.syntax().clone())
    } else if let Some(loop_expr) = ast::LoopExpr::cast(node.clone()) {
        loop_expr.loop_body().map(|body| body.syntax().clone())
    } else if let Some(closure) = ast::ClosureExpr::cast(node.clone()) {
        closure.body().map(|body| body.syntax().clone())
    } else {
        ast::MatchArm::cast(node.clone())
            .and_then(|arm| arm.expr())
            .map(|body| body.syntax().clone())
    }
}

pub(crate) fn arguments(node: &ra_ap_syntax::SyntaxNode) -> Option<ast::ArgList> {
    ast::CallExpr::cast(node.clone())
        .and_then(|call| call.arg_list())
        .or_else(|| ast::MethodCallExpr::cast(node.clone()).and_then(|call| call.arg_list()))
}
