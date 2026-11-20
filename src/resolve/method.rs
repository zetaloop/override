use std::collections::HashSet;

use ra_ap_syntax::{
    AstNode, Edition, SyntaxKind, ast,
    ast::{HasName, HasTypeBounds},
};

use crate::{
    Result, fragment,
    source::Location,
    symbol::{self, Binding, Namespace},
};

use super::{ident, path_name};

#[derive(Clone)]
pub(crate) struct Target {
    owner: ast::Type,
    trait_: Option<ast::Path>,
    name: String,
}

impl Target {
    pub fn parse(path: &str, name: &str, edition: Edition) -> Result<Self> {
        let path = fragment::path(path, edition)?;
        let method = path
            .segment()
            .and_then(|s| s.name_ref())
            .ok_or("expected a method name")?;
        if ident(method.text()) != ident(name) {
            return Err(format!("`{path}` does not name the selected method `{name}`").into());
        }
        let owner = path
            .qualifier()
            .ok_or("a method binding requires its owning type")?;
        let (owner, trait_) = if let Some(anchor) = owner.segment().and_then(|s| s.type_anchor()) {
            (
                anchor
                    .self_ty()
                    .ok_or("method binding has no receiver type")?,
                anchor.trait_().and_then(|t| t.path()),
            )
        } else {
            (fragment::ty(&owner.to_string(), edition)?, None)
        };
        Ok(Self {
            owner,
            trait_,
            name: name.into(),
        })
    }
}

pub(crate) struct Method {
    pub declaration: Option<Location>,
    pub trait_: Option<(Binding, String)>,
    owner: Ty,
}

impl Method {
    pub fn matches(&self, target: &Location, unresolved: Option<&str>, name: &str) -> bool {
        let trait_decl = self
            .trait_
            .as_ref()
            .and_then(|(b, _)| member(&b.declaration, name));
        if let Some(path) = unresolved {
            let Some((owner, member)) = path.rsplit_once("::") else {
                return false;
            };
            let binding = self.trait_.as_ref().map_or(&self.owner.nominal, |(t, _)| t);
            ident(member) == ident(name) && binding.unresolved.as_deref() == Some(owner)
        } else {
            self.declaration
                .iter()
                .chain(trait_decl.iter())
                .any(|d| d.same(target))
        }
    }
}

// References are stored from the nominal type outwards: `&mut &T` ends in `Mut`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ref {
    Shared,
    Mut,
}

#[derive(Clone)]
struct Ty {
    nominal: Binding,
    arguments: String,
    refs: Vec<Ref>,
}

impl Ty {
    fn same(&self, other: &Self) -> bool {
        self.nominal.same_target(&other.nominal)
            && self.arguments == other.arguments
            && self.refs == other.refs
    }

    fn parse<'a>(
        defs: &[Location],
        at: &'a Location,
        parents: impl Iterator<Item = &'a Location> + Clone,
        mut ty: ast::Type,
    ) -> Option<Self> {
        let mut context = at.clone();
        let mut aliases = HashSet::new();
        let mut refs = Vec::new();
        loop {
            match ty {
                ast::Type::RefType(t) => {
                    refs.push(if t.mut_token().is_some() {
                        Ref::Mut
                    } else {
                        Ref::Shared
                    });
                    ty = t.ty()?;
                }
                ast::Type::ParenType(t) => ty = t.ty()?,
                ast::Type::PathType(t) => {
                    let path = t.path()?;
                    let bindings = if aliases.is_empty() {
                        symbol::scoped(
                            defs,
                            at,
                            parents.clone(),
                            &path_name(&path),
                            Namespace::Type,
                        )
                        .0
                    } else {
                        symbol::lookup(
                            defs,
                            &context,
                            &path_name(&path),
                            Some(Namespace::Type),
                            &mut HashSet::new(),
                        )
                    };
                    let [binding] = bindings.as_slice() else {
                        return None;
                    };
                    let arguments = path
                        .segment()?
                        .syntax()
                        .children()
                        .find_map(ast::GenericArgList::cast)
                        .map(|args| fragment::spelling(args.syntax()))
                        .unwrap_or_default();
                    if binding.unresolved.is_none()
                        && let Some(alias) = ast::TypeAlias::cast(binding.declaration.node.clone())
                    {
                        if !arguments.is_empty() || !aliases.insert(alias.syntax().clone()) {
                            return None;
                        }
                        ty = alias.ty()?;
                        context = binding.declaration.clone();
                    } else {
                        refs.reverse();
                        return Some(Self {
                            nominal: binding.clone(),
                            arguments,
                            refs,
                        });
                    }
                }
                _ => return None,
            }
        }
    }

    fn candidates(&self) -> Vec<Self> {
        let mut result = Vec::new();
        let mut ty = self.clone();
        loop {
            result.push(ty.clone());
            for borrow in [Ref::Shared, Ref::Mut] {
                let mut borrowed = ty.clone();
                borrowed.refs.push(borrow);
                result.push(borrowed);
            }
            if ty.refs.pop().is_none() {
                break;
            }
        }
        result
    }
}

fn receiver(
    defs: &[Location],
    at: &Location,
    parents: &[&Location],
    expr: &ast::Expr,
) -> Option<Ty> {
    match expr {
        ast::Expr::MethodCallExpr(call) => {
            let method = resolve(
                defs,
                at,
                parents.iter().copied(),
                call,
                call.name_ref()?.text(),
                None,
            )
            .ok()??;
            let decl = method.declaration.as_ref()?;
            let result = ast::Fn::cast(decl.node.clone())?.ret_type()?.ty()?;
            associated_type(defs, &method.owner, decl, result)
        }
        ast::Expr::CallExpr(call) => {
            let ast::Expr::PathExpr(callee) = call.expr()? else {
                return None;
            };
            let path = callee.path()?;
            let (bindings, _) = symbol::scoped(
                defs,
                at,
                parents.iter().copied(),
                &path_name(&path),
                Namespace::Value,
            );
            let [b] = bindings.as_slice() else {
                return None;
            };
            if b.unresolved.is_some() {
                return None;
            }
            if let Some(f) = ast::Fn::cast(b.declaration.node.clone()) {
                Ty::parse(
                    defs,
                    &b.declaration,
                    std::iter::empty(),
                    f.ret_type()?.ty()?,
                )
            } else {
                let ast::FieldList::TupleFieldList(_) =
                    ast::Struct::cast(b.declaration.node.clone())?.field_list()?
                else {
                    return None;
                };
                Some(Ty {
                    nominal: b.clone(),
                    arguments: String::new(),
                    refs: Vec::new(),
                })
            }
        }
        ast::Expr::ParenExpr(e) => receiver(defs, at, parents, &e.expr()?),
        ast::Expr::RefExpr(e) => {
            let mut ty = receiver(defs, at, parents, &e.expr()?)?;
            ty.refs.push(if e.mut_token().is_some() {
                Ref::Mut
            } else {
                Ref::Shared
            });
            Some(ty)
        }
        ast::Expr::CastExpr(e) => Ty::parse(defs, at, parents.iter().copied(), e.ty()?),
        ast::Expr::RecordExpr(e) => Ty::parse(
            defs,
            at,
            parents.iter().copied(),
            fragment::ty(&e.path()?.to_string(), at.edition).ok()?,
        ),
        ast::Expr::PathExpr(e) => {
            let name = path_name(&e.path()?);
            let (bindings, site) =
                symbol::scoped(defs, at, parents.iter().copied(), &name, Namespace::Value);
            let [b] = bindings.as_slice() else {
                return None;
            };
            if b.unresolved.is_some() {
                return None;
            }
            let decl = &b.declaration;
            let remaining = parents
                .iter()
                .position(|p| std::ptr::eq(*p, site))
                .map_or(parents, |i| &parents[i + 1..]);
            match decl.node.kind() {
                SyntaxKind::STRUCT
                    if ast::Struct::cast(decl.node.clone())?.field_list().is_none() =>
                {
                    Some(Ty {
                        nominal: b.clone(),
                        arguments: String::new(),
                        refs: Vec::new(),
                    })
                }
                SyntaxKind::SELF_PARAM => {
                    let function = decl.ancestors().into_iter().find_map(ast::Fn::cast)?;
                    let implementation = decl.ancestors().into_iter().find_map(ast::Impl::cast)?;
                    let owner = Ty::parse(
                        defs,
                        decl,
                        remaining.iter().copied(),
                        implementation.self_ty()?,
                    )?;
                    parameter(defs, &owner, &decl.at(function.syntax().clone()))
                }
                SyntaxKind::IDENT_PAT => {
                    let parent = decl.node.parent()?;
                    if let Some(param) = ast::Param::cast(parent.clone()) {
                        return Ty::parse(defs, decl, remaining.iter().copied(), param.ty()?);
                    }
                    let binding = ast::LetStmt::cast(parent)?;
                    if let Some(ty) = binding.ty() {
                        Ty::parse(defs, decl, remaining.iter().copied(), ty)
                    } else {
                        let value = binding.initializer()?;
                        receiver(defs, &decl.at(value.syntax().clone()), remaining, &value)
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn member(owner: &Location, name: &str) -> Option<Location> {
    let list = owner.node.children().find_map(ast::AssocItemList::cast)?;
    let f = list
        .syntax()
        .children()
        .filter_map(ast::Fn::cast)
        .find(|f| f.name().is_some_and(|n| ident(n.text()) == ident(name)))?;
    Some(owner.at(f.syntax().clone()))
}

fn parameter(defs: &[Location], owner: &Ty, f: &Location) -> Option<Ty> {
    let param = ast::Fn::cast(f.node.clone())?.param_list()?.self_param()?;
    if let Some(explicit) = param.ty() {
        return associated_type(defs, owner, f, explicit);
    }
    let mut ty = owner.clone();
    if param.amp_token().is_some() {
        ty.refs.push(if param.mut_token().is_some() {
            Ref::Mut
        } else {
            Ref::Shared
        });
    }
    Some(ty)
}

fn associated_type(defs: &[Location], owner: &Ty, decl: &Location, ty: ast::Type) -> Option<Ty> {
    match ty {
        ast::Type::RefType(t) => {
            let mut inner = associated_type(defs, owner, decl, t.ty()?)?;
            inner.refs.push(if t.mut_token().is_some() {
                Ref::Mut
            } else {
                Ref::Shared
            });
            Some(inner)
        }
        ast::Type::PathType(ref t) if t.path().is_some_and(|p| path_name(&p) == "Self") => {
            Some(owner.clone())
        }
        ty => Ty::parse(defs, decl, std::iter::empty(), ty),
    }
}

fn trait_binding<'a>(
    defs: &[Location],
    context: &Location,
    path: &ast::Path,
    scopes: impl Iterator<Item = &'a Location>,
) -> Option<(Binding, String)> {
    let bindings = symbol::lookup(
        defs,
        context,
        &path_name(path),
        Some(Namespace::Type),
        &mut HashSet::new(),
    );
    let [target] = bindings.as_slice() else {
        return None;
    };
    let name = if target.unresolved.is_none() {
        ast::Trait::cast(target.declaration.node.clone())?
            .name()?
            .to_string()
    } else {
        path.segment()?.name_ref()?.to_string()
    };
    let mut scopes = scopes;
    let binding = scopes.find_map(|at| symbol::in_scope_trait(defs, at, target, &name))?;
    Some((binding, name))
}

pub(crate) fn resolve<'a>(
    defs: &[Location],
    at: &'a Location,
    parents: impl Iterator<Item = &'a Location> + Clone,
    call: &ast::MethodCallExpr,
    name: &str,
    explicit: Option<&Target>,
) -> Result<Option<Method>> {
    let scopes = std::iter::once(at).chain(parents.clone());
    let bound = if let Some(target) = explicit {
        let context = scopes.clone().last().unwrap_or(at);
        let owner = Ty::parse(defs, context, std::iter::empty(), target.owner.clone())
            .ok_or_else(|| format!("cannot resolve bound receiver `{}`", target.owner))?;
        let trait_ = target
            .trait_
            .as_ref()
            .map(|path| {
                trait_binding(defs, context, path, scopes.clone())
                    .ok_or_else(|| format!("bound trait `{path}` is unavailable at `{call}`"))
            })
            .transpose()?;
        let function = trait_
            .as_ref()
            .and_then(|(b, _)| member(&b.declaration, &target.name));
        Some((owner, trait_, function))
    } else {
        None
    };
    let actual = match &bound {
        Some((ty, _, _)) => ty.clone(),
        None => match call
            .receiver()
            .and_then(|e| receiver(defs, at, &parents.clone().collect::<Vec<_>>(), &e))
        {
            Some(ty) => ty,
            None => return Ok(None),
        },
    };
    let types = actual.candidates();
    let mut candidates = Vec::new();
    let mut unknown = None;
    for decl in defs.iter().filter(|d| d.node.kind() == SyntaxKind::IMPL) {
        let implementation = ast::Impl::cast(decl.node.clone()).unwrap();
        let Some(owner) = implementation
            .self_ty()
            .and_then(|t| Ty::parse(defs, decl, std::iter::empty(), t))
        else {
            continue;
        };
        if !owner.nominal.same_target(&actual.nominal) || owner.arguments != actual.arguments {
            continue;
        }
        let trait_ = match implementation.trait_() {
            Some(ast::Type::PathType(t)) => {
                let Some(b) = t
                    .path()
                    .and_then(|p| trait_binding(defs, decl, &p, scopes.clone()))
                else {
                    continue;
                };
                Some(b)
            }
            Some(_) => continue,
            None => None,
        };
        if let Some((owner_bound, t, _)) = &bound
            && (!owner.same(owner_bound)
                || !match (t, &trait_) {
                    (None, None) => true,
                    (Some((a, _)), Some((b, _))) => a.same_target(b),
                    _ => false,
                })
        {
            continue;
        }
        let Some(function) = member(decl, name).or_else(|| {
            trait_
                .as_ref()
                .and_then(|(b, _)| member(&b.declaration, name))
        }) else {
            if bound.is_none() && trait_.as_ref().is_some_and(|(b, _)| b.unresolved.is_some()) {
                let mut shared = owner.clone();
                shared.refs.push(Ref::Shared);
                let mut mutable = owner.clone();
                mutable.refs.push(Ref::Mut);
                if let Some(rank) = types
                    .iter()
                    .position(|t| t.same(&owner) || t.same(&shared) || t.same(&mutable))
                {
                    let rank = (rank, true);
                    unknown = Some(unknown.map_or(rank, |old| std::cmp::min(old, rank)));
                }
            }
            continue;
        };
        let Some(self_ty) = parameter(defs, &owner, &function) else {
            continue;
        };
        let rank = if bound.is_some() {
            0
        } else {
            let Some(rank) = types.iter().position(|ty| ty.same(&self_ty)) else {
                continue;
            };
            rank
        };
        candidates.push((
            (rank, trait_.is_some()),
            Method {
                declaration: Some(function),
                owner,
                trait_,
            },
        ));
    }
    // A type parameter's bounds make their methods available without a `use`.
    if actual.nominal.declaration.node.kind() == SyntaxKind::TYPE_PARAM && bound.is_none() {
        let param = ast::TypeParam::cast(actual.nominal.declaration.node.clone()).unwrap();
        for ty in param
            .type_bound_list()
            .into_iter()
            .flat_map(|l| l.bounds())
            .filter_map(|b| b.ty())
        {
            let ast::Type::PathType(t) = ty else { continue };
            let Some(p) = t.path() else { continue };
            let bindings = symbol::lookup(
                defs,
                &actual.nominal.declaration,
                &path_name(&p),
                Some(Namespace::Type),
                &mut HashSet::new(),
            );
            if let [b] = bindings.as_slice()
                && let Some(f) = member(&b.declaration, name)
                && let Some(t) = parameter(defs, &actual, &f)
                && let Some(rank) = types.iter().position(|ty| ty.same(&t))
            {
                candidates.push((
                    (rank, false),
                    Method {
                        declaration: Some(f),
                        owner: actual.clone(),
                        trait_: None,
                    },
                ));
            }
        }
    }
    if let Some(best) = candidates.iter().map(|(rank, _)| *rank).min() {
        if unknown.is_some_and(|rank| rank <= best) {
            return Ok(None);
        }
        let mut chosen = candidates
            .into_iter()
            .filter(|(rank, _)| *rank == best)
            .map(|(_, m)| m);
        let method = chosen.next().unwrap();
        if chosen.next().is_some() {
            return Err(
                format!("method `{call}` is ambiguous; bind the call to its declaration").into(),
            );
        }
        return Ok(Some(method));
    }
    Ok(bound.map(|(owner, trait_, declaration)| Method {
        owner,
        trait_,
        declaration,
    }))
}
