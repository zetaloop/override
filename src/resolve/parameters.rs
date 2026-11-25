use std::collections::HashMap;

use ra_ap_syntax::{AstNode, SyntaxNode, ast};

use crate::{Result, Source, fragment, source::Location};

#[derive(PartialEq)]
enum Condition {
    Flag(usize),
    All(Vec<Self>),
    Any(Vec<Self>),
    Not(Box<Self>),
}

impl Condition {
    fn predicate(p: ast::CfgPredicate, flag: &mut impl FnMut(String) -> usize) -> Self {
        match &p {
            ast::CfgPredicate::CfgAtom(atom) if atom.true_token().is_some() => Self::All(vec![]),
            ast::CfgPredicate::CfgAtom(atom) if atom.false_token().is_some() => Self::Any(vec![]),
            ast::CfgPredicate::CfgComposite(c) => {
                let mut parts = c.cfg_predicates().map(|p| Self::predicate(p, flag));
                match c.keyword().as_ref().map(|k| k.text()) {
                    Some("all") => return Self::All(parts.collect()),
                    Some("any") => return Self::Any(parts.collect()),
                    Some("not") => {
                        if let Some(p) = parts.next() {
                            return Self::Not(Box::new(p));
                        }
                    }
                    _ => {}
                }
                Self::Flag(flag(fragment::spelling(p.syntax())))
            }
            _ => Self::Flag(flag(fragment::spelling(p.syntax()))),
        }
    }

    fn meta(meta: ast::Meta, flag: &mut impl FnMut(String) -> usize) -> Option<Self> {
        match meta {
            ast::Meta::CfgMeta(c) => Some(Self::predicate(c.cfg_predicate()?, flag)),
            ast::Meta::CfgAttrMeta(c) => {
                let inner = c
                    .metas()
                    .filter_map(|m| Self::meta(m, flag))
                    .collect::<Vec<_>>();
                if inner.is_empty() {
                    return None;
                }
                Some(Self::Any(vec![
                    Self::Not(Box::new(Self::predicate(c.cfg_predicate()?, flag))),
                    Self::All(inner),
                ]))
            }
            _ => None,
        }
    }

    fn enabled(&self, facts: &[Flag], unknown: &mut Option<usize>) -> Option<bool> {
        match self {
            Self::Flag(i) => {
                let fact = &facts[*i];
                if fact.value.is_none() && fact.shared {
                    *unknown = Some(*i);
                }
                fact.value
            }
            Self::Not(p) => p.enabled(facts, unknown).map(|v| !v),
            Self::Any(parts) | Self::All(parts) => {
                let any = matches!(self, Self::Any(_));
                let mut result = Some(!any);
                let mut relevant = None;
                for p in parts {
                    match p.enabled(facts, &mut relevant) {
                        Some(v) if v == any => return Some(any),
                        None => result = None,
                        _ => {}
                    }
                }
                if result.is_none() && relevant.is_some() {
                    *unknown = relevant;
                }
                result
            }
        }
    }
}

#[derive(Default)]
struct Flag {
    shared: bool,
    value: Option<bool>,
}

fn feasible<const N: usize>(
    required: &[&Condition],
    ranges: [(&[Condition], &[Condition]); N],
    facts: &mut [Flag],
) -> bool {
    let mut unknown = None;
    if required
        .iter()
        .any(|p| p.enabled(facts, &mut unknown) == Some(false))
    {
        return false;
    }
    for (p, a) in ranges {
        if p == a {
            continue;
        }
        let mut counts = |xs: &[Condition]| {
            xs.iter()
                .fold((0, 0), |(lo, hi), x| match x.enabled(facts, &mut unknown) {
                    Some(true) => (lo + 1, hi + 1),
                    Some(false) => (lo, hi),
                    None => (lo, hi + 1),
                })
        };
        let (pmin, pmax) = counts(p);
        let (amin, amax) = counts(a);
        if pmin > amax || amin > pmax {
            return false;
        }
    }
    // Single-use flags vary independently. Only repeated flags can couple the two counts.
    let Some(k) = unknown else { return true };
    let found = [false, true].into_iter().any(|v| {
        facts[k].value = Some(v);
        feasible(required, ranges, facts)
    });
    facts[k].value = None;
    found
}

pub(super) fn corresponding(
    params: &[SyntaxNode],
    args: &[ast::Expr],
    selected: &[usize],
    source: &Source,
    declaration: &Location,
    call: &Location,
) -> Result<Option<usize>> {
    if selected.is_empty() {
        return Ok(None);
    }
    let mut names = HashMap::<_, usize>::new();
    let mut facts = Vec::<Flag>::new();
    let mut condition = |node: &SyntaxNode, external: bool| {
        Condition::All(
            node.children()
                .filter_map(ast::Attr::cast)
                .filter_map(|a| a.meta())
                .filter_map(|m| {
                    Condition::meta(m, &mut |name| {
                        *names
                            .entry((external, name))
                            .and_modify(|i| facts[*i].shared = true)
                            .or_insert_with(|| {
                                facts.push(Flag::default());
                                facts.len() - 1
                            })
                    })
                })
                .collect(),
        )
    };
    let root = declaration
        .parents
        .first()
        .map_or(&declaration.root, |f| &f.root);
    let external = declaration.krate != call.krate || !source.roots().any(|r| &r.root == root);
    let params = params
        .iter()
        .map(|p| condition(p, external))
        .collect::<Vec<_>>();
    let args = args
        .iter()
        .map(|a| condition(a.syntax(), false))
        .collect::<Vec<_>>();
    if params == args {
        return match selected {
            [i] => Ok(Some(*i)),
            _ => Err("argument selection matches several parameters".into()),
        };
    }
    let scope = Condition::All(
        declaration
            .ancestors()
            .into_iter()
            .map(|n| (n, external))
            .chain(call.ancestors().into_iter().map(|n| (n, false)))
            .map(|(n, external)| condition(&n, external))
            .collect(),
    );
    // Both sides of a corresponding slot have the same active prefix and suffix lengths.
    let mut possible = |i: usize, j: usize| {
        feasible(
            &[&scope, &params[i], &args[j]],
            [
                (&params[..i], &args[..j]),
                (&params[i + 1..], &args[j + 1..]),
            ],
            &mut facts,
        )
    };
    let ambiguous = "conditional parameters do not identify a unique argument";
    let mut found = None;
    for &i in selected {
        for j in 0..args.len() {
            if Some(j) != found && possible(i, j) {
                if found.is_some() {
                    return Err(ambiguous.into());
                }
                found = Some(j);
            }
        }
    }
    if let Some(j) = found
        && (0..params.len()).any(|i| !selected.contains(&i) && possible(i, j))
    {
        return Err(ambiguous.into());
    }
    if found.is_none() && !feasible(&[&scope], [(&params, &args)], &mut facts) {
        return Err("call and declaration have different argument counts".into());
    }
    Ok(found)
}
