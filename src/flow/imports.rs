use std::collections::HashSet;

use ra_ap_syntax::{
    AstNode, ast,
    ast::{HasAttrs, HasName},
};

use crate::{
    Result, resolve,
    source::Location,
    symbol::{self, Binding, Namespace},
};

pub(crate) struct Imports {
    moved: Location,
    pub destination: Location,
    pub uses: Vec<String>,
    pub has_local_uses: bool,
    scope: Option<crate::expand::Scope>,
}

impl Imports {
    pub fn new(source: &crate::Source, moved: &Location) -> Result<Self> {
        let owner = moved
            .ancestors()
            .into_iter()
            .find_map(ast::Fn::cast)
            .ok_or("extraction requires an enclosing function")?;
        let destination = moved.at(owner.syntax().clone());
        let has_local_uses = owner
            .syntax()
            .descendants()
            .filter(|n| ast::Use::can_cast(n.kind()))
            .any(|n| !moved.contains(&destination.at(n)));
        let scope = owner
            .syntax()
            .descendants()
            .any(|n| ast::MacroCall::can_cast(n.kind()))
            .then(|| crate::expand::Scope::new(source, &destination))
            .flatten();
        let has_local_uses = has_local_uses || scope.is_some();
        Ok(Self {
            scope,
            moved: moved.clone(),
            destination,
            uses: Vec::new(),
            has_local_uses,
        })
    }

    fn lost(&self, item: &Location) -> bool {
        self.destination.contains(item) && !self.moved.contains(item)
    }

    pub fn query(
        &mut self,
        definitions: &[Location],
        at: ra_ap_syntax::TextRange,
        name: &str,
    ) -> bool {
        let Some(scope) = &self.scope else {
            return false;
        };
        let mut imported = None;
        let resolved=scope.reference(at,name,|tree,position|{
                let lookup = |path: &str| {
                    symbol::lookup(
                        definitions,
                        &self.destination,
                        path,
                        Some(Namespace::Value),
                        &mut HashSet::new(),
                    )
                };
                let mut target = resolve::import_path(&tree);
                if tree.star_token().is_some() {
                    target = format!("{target}::{name}");
                }
                if !self.moved.range().contains_range(position)
                    && !matches!((lookup(name).as_slice(),lookup(&target).as_slice()),([a],[b]) if a.same_target(b))
                {
                    imported=Some(tree);
                }
        });
        if let Some(tree) = imported {
            self.import(&tree, name);
        }
        resolved
    }

    pub fn reference<'a>(
        &mut self,
        definitions: &[Location],
        at: &'a Location,
        parents: impl Iterator<Item = &'a Location>,
        name: &str,
        namespace: Namespace,
    ) -> Result<()> {
        let (bindings, _) = symbol::scoped(definitions, at, parents, name, namespace);
        if let [b] = bindings.as_slice() {
            self.binding(name, b, |dest| matches!(
                symbol::lookup(definitions, dest, name, Some(namespace), &mut HashSet::new()).as_slice(),
                [other] if b.same_target(other)
            ))?;
        }
        Ok(())
    }

    pub fn visit<'a>(
        &mut self,
        definitions: &[Location],
        at: &'a Location,
        parents: impl Iterator<Item = &'a Location> + Clone,
        explicit: Option<&resolve::method::Target>,
        origin: Option<ra_ap_syntax::TextRange>,
    ) -> Result<()> {
        if let Some(path) = ast::Path::cast(at.node.clone())
            && let Some(namespace) = symbol::path_namespace(&path)
            && namespace != Namespace::Macro
        {
            let name = resolve::path_name(&path);
            if namespace != Namespace::Value
                || path.qualifier().is_some()
                || !origin.is_some_and(|r| self.query(definitions, r, &name))
            {
                self.reference(definitions, at, parents, &name, namespace)?;
            }
        } else if let Some(call) = ast::MethodCallExpr::cast(at.node.clone()) {
            let name = call.name_ref().ok_or("method call has no name")?;
            match resolve::method::resolve(
                definitions,
                at,
                parents.clone(),
                &call,
                name.text(),
                explicit,
            )? {
                Some(method) => {
                    if let Some((b, name)) = method.trait_ {
                        self.binding(&name, &b, |dest| {
                            symbol::in_scope_trait(definitions, dest, &b, &name).is_some()
                        })?;
                    }
                }
                None => {
                    for site in std::iter::once(at).chain(parents) {
                        if let Some(import) = symbol::method_import(
                            definitions,
                            site,
                            name.text(),
                            |leaf| self.lost(leaf),
                            &self.destination,
                        ) {
                            return Err(format!("cannot determine whether `{call}` uses `{import}`; supply its declarations or use Source::bind_method").into());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn binding(
        &mut self,
        path: &str,
        binding: &Binding,
        same_at_destination: impl FnOnce(&Location) -> bool,
    ) -> Result<()> {
        let local_macro = ast::Macro::can_cast(binding.declaration.node.kind())
            && self.lost(&binding.declaration);
        if !local_macro && !binding.imports.iter().any(|i| self.lost(&i.leaf)) {
            return Ok(());
        }
        if same_at_destination(&self.destination) || self.moved.contains(&binding.declaration) {
            return Ok(());
        }
        if self.carry(binding)? || !local_macro {
            return Ok(());
        }
        let name = binding
            .declaration
            .node
            .children()
            .find_map(ast::Name::cast);
        Err(format!("`{path}!` uses the local macro `{}` at {:?}, outside the extracted code and not visible from the helper",
            name.map(|n| n.to_string()).unwrap_or_else(||path.into()), binding.declaration.range()).into())
    }

    fn carry(&mut self, binding: &Binding) -> Result<bool> {
        let mut supplied = false;
        for import in &binding.imports {
            if !self.lost(&import.leaf) {
                continue;
            }
            let tree = ast::UseTree::cast(import.leaf.node.clone()).ok_or("expected an import")?;
            if tree.star_token().is_some() && binding.unresolved.is_some() {
                continue;
            }
            self.import(&tree, &import.name);
            supplied = true;
        }
        Ok(supplied)
    }

    fn import(&mut self, tree: &ast::UseTree, name: &str) {
        let mut path = resolve::import_path(tree);
        if tree.star_token().is_some() {
            let prefix = if ra_ap_syntax::utils::is_raw_identifier(name, self.destination.edition) {
                "r#"
            } else {
                ""
            };
            path = format!("{path}::{prefix}{name}");
        } else if let Some(rename) = tree.rename() {
            path = format!(
                "{path} as {}",
                rename.name().map_or_else(|| "_".into(), |n| n.to_string())
            );
        }
        let item = tree
            .syntax()
            .ancestors()
            .find_map(ast::Use::cast)
            .expect("import tree belongs to a use declaration");
        let attrs: String = item.attrs().map(|a| format!("{a}\n")).collect();
        let decl = format!("{attrs}use {path};");
        if !self.uses.contains(&decl) {
            self.uses.push(decl)
        }
    }
}
