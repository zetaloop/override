use std::fmt;

use crate::{Source, source::Location};
use base_db::{
    CrateGraphBuilder, CrateOrigin, CrateWorkspaceData, CratesMap, FileChange, FileId, FileSet,
    FileSourceRootInput, FileText, Files, Nonce, SourceDatabase, SourceRoot, SourceRootId,
    SourceRootInput, TargetKind, VfsPath,
};
use hir_def::{
    AssocItemId, DefWithBodyId, Lookup, ModuleDefId,
    expr_store::{Body, HygieneId, path::Path, scope::ExprScopes},
    hir::{Expr, ExprId},
    item_scope::ImportOrGlob,
    nameres::{block_def_map, crate_def_map},
    resolver::{ResolveValueResult, ValueNs, resolver_for_scope},
    src::{HasChildSource, HasSource},
};
use hir_expand::{
    InFile,
    mod_path::{ModPath, PathKind},
    name::Name,
};
use ra_ap_syntax::{AstNode, TextRange, TextSize, ast};
use salsa::Durability;
use triomphe::Arc;

#[salsa::db]
#[derive(Clone, Default)]
struct Db {
    storage: salsa::Storage<Self>,
    files: Arc<Files>,
    crates: Arc<CratesMap>,
    nonce: Nonce,
}

impl fmt::Debug for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Source")
    }
}
#[salsa::db]
impl salsa::Database for Db {}
#[salsa::db]
impl SourceDatabase for Db {
    fn file_text(&self, id: FileId) -> FileText {
        self.files.file_text(id)
    }
    fn set_file_text(&mut self, id: FileId, text: &str) {
        self.files.clone().set_file_text(self, id, text)
    }
    fn set_file_text_with_durability(&mut self, id: FileId, text: &str, d: Durability) {
        self.files
            .clone()
            .set_file_text_with_durability(self, id, text, d)
    }
    fn source_root(&self, id: SourceRootId) -> SourceRootInput {
        self.files.source_root(id)
    }
    fn set_source_root_with_durability(
        &mut self,
        id: SourceRootId,
        root: Arc<SourceRoot>,
        d: Durability,
    ) {
        self.files
            .clone()
            .set_source_root_with_durability(self, id, root, d)
    }
    fn file_source_root(&self, id: FileId) -> FileSourceRootInput {
        self.files.file_source_root(self, id)
    }
    fn set_file_source_root_with_durability(
        &mut self,
        id: FileId,
        root: SourceRootId,
        d: Durability,
    ) {
        self.files
            .clone()
            .set_file_source_root_with_durability(self, id, root, d)
    }
    fn crates_map(&self) -> Arc<CratesMap> {
        self.crates.clone()
    }
    fn nonce_and_revision(&self) -> (Nonce, salsa::Revision) {
        (
            self.nonce,
            salsa::plumbing::ZalsaDatabase::zalsa(self).current_revision(),
        )
    }
    fn line_column(&self, id: FileId, offset: TextSize) -> Result<(u32, u32), ()> {
        let text = self.file_text(id).text(self);
        let before = text.get(..usize::from(offset)).ok_or(())?;
        Ok((
            before.bytes().filter(|b| *b == b'\n').count() as u32,
            before.rsplit('\n').next().unwrap_or("").chars().count() as u32,
        ))
    }
}

pub(crate) struct Scope {
    db: Db,
    owner: DefWithBodyId,
    expressions: Vec<(TextRange, ExprId)>,
}

enum Value {
    Local,
    Import(ast::UseTree, TextRange),
}

impl Scope {
    pub fn new(source: &Source, owner: &Location) -> Option<Self> {
        let mut db = Db::default();
        base_db::set_all_crates_with_durability(&mut db, [], Durability::HIGH);
        base_db::LibraryRoots::new(&db, Default::default());
        base_db::LocalRoots::new(&db, Default::default());
        hir_def::set_expand_proc_attr_macros(&mut db, false);
        let mut files = FileSet::default();
        let mut change = FileChange::default();
        let mut root = None;
        for (i, file) in source.roots().enumerate() {
            let id = FileId::from_raw(i as u32);
            let path = if file.module.is_empty() {
                root = Some(id);
                "/lib.rs".into()
            } else {
                format!("/{}.rs", file.module.replace("::", "/"))
            };
            files.insert(id, VfsPath::new_virtual_path(path));
            change.change_file(id, Some(file.root.to_string()));
        }
        change.set_roots(vec![SourceRoot::new_local(files)]);
        let mut graph = CrateGraphBuilder::default();
        let krate = graph.add_crate_root(
            root.unwrap_or(FileId::from_raw(0)),
            source.edition(),
            None,
            None,
            Default::default(),
            None,
            Default::default(),
            CrateOrigin::Local {
                repo: None,
                name: None,
            },
            vec![],
            TargetKind::Lib {
                is_proc_macro: false,
            },
            Arc::new(base_db::AbsPathBuf::assert_utf8(
                std::env::current_dir().ok()?,
            )),
            Arc::new(CrateWorkspaceData {
                target: Err("source has no compilation target".into()),
                toolchain: None,
            }),
        );
        change.set_crate_graph(graph);
        let krate = change.apply(&mut db)?[&krate];
        for (_, m) in crate_def_map(&db, krate).modules() {
            let mut functions = Vec::new();
            let mut associated = |items: &[(Name, AssocItemId)]| {
                functions.extend(items.iter().filter_map(|(_, a)| {
                    if let AssocItemId::FunctionId(f) = a {
                        Some(*f)
                    } else {
                        None
                    }
                }))
            };
            for i in m.scope.impls() {
                associated(&i.impl_items(&db).items)
            }
            for t in m.scope.declarations() {
                if let ModuleDefId::TraitId(t) = t {
                    associated(&t.trait_items(&db).items)
                }
            }
            functions.extend(m.scope.declarations().filter_map(|d| {
                if let ModuleDefId::FunctionId(f) = d {
                    Some(f)
                } else {
                    None
                }
            }));
            for f in functions {
                let loc = f.lookup(&db).source(&db);
                if loc
                    .file_id
                    .file_id()
                    .is_some_and(|id| id.file_id(&db) == FileId::from_raw(0))
                    && loc.value.syntax().text_range() == owner.range()
                {
                    let owner = f.into();
                    let (body, map) = Body::with_source_map(&db, owner);
                    let expressions = body
                        .exprs()
                        .filter_map(|(id, _)| {
                            let src = map.expr_syntax(id).ok()?;
                            let node = src.value.to_node(&src.file_id.parse_or_expand(&db));
                            let r = InFile::new(src.file_id, node.syntax())
                                .original_file_range_with_macro_call_input(&db);
                            Some((r.range, id))
                        })
                        .collect();
                    return Some(Self {
                        db: db.clone(),
                        owner,
                        expressions,
                    });
                }
            }
        }
        None
    }

    pub fn reference(
        &self,
        at: TextRange,
        name: &str,
        carry: impl FnOnce(ast::UseTree, TextRange),
    ) -> bool {
        match self.value(at, name) {
            Some(Value::Local) => true,
            Some(Value::Import(tree, range)) => {
                carry(tree, range);
                true
            }
            None => false,
        }
    }

    fn value(&self, at: TextRange, name: &str) -> Option<Value> {
        let db = &self.db;
        let body = Body::of(db, self.owner);
        let scopes = ExprScopes::of(db, self.owner);
        let name = Name::new_root(name.trim_start_matches("r#"));
        let (id,_,exact)=self.expressions.iter().filter_map(|&(r,id)|{
            if !r.contains_range(at){return None}
            let exact=matches!(&body[id],Expr::Path(p) if p.mod_path().is_some_and(|p|p.segments()==std::slice::from_ref(&name)));
            Some((id,r.len(),exact))
        }).min_by_key(|(_,len,exact)|(!exact,*len))?;
        let scope = scopes.scope_for(id);
        let resolver = resolver_for_scope(db, self.owner, scope);
        let path = Path::from_known_path_with_no_generic(ModPath::from_segments(
            PathKind::Plain,
            [name.clone()],
        ));
        let hygiene = if exact {
            body.expr_path_hygiene(id)
        } else {
            HygieneId::ROOT
        };
        match resolver.resolve_path_in_value_ns(db, &path, hygiene)? {
            ResolveValueResult::ValueNs(ValueNs::LocalBinding(_)) => Some(Value::Local),
            ResolveValueResult::ValueNs(value) => {
                let def = match value {
                    ValueNs::FunctionId(f) => ModuleDefId::FunctionId(f),
                    ValueNs::ConstId(c) => ModuleDefId::ConstId(c),
                    ValueNs::StaticId(s) => ModuleDefId::StaticId(s),
                    ValueNs::StructId(s) => ModuleDefId::AdtId(s.into()),
                    ValueNs::EnumVariantId(v) => ModuleDefId::EnumVariantId(v),
                    _ => return None,
                };
                for scope in scopes.scope_chain(scope) {
                    let Some(block) = scopes.block(scope) else {
                        continue;
                    };
                    let blocks = block_def_map(db, block);
                    let Some(item) = blocks[blocks.root_module_id()].scope.get(&name).values else {
                        continue;
                    };
                    if item.def != def {
                        continue;
                    }
                    let (u, i) = match item.import? {
                        ImportOrGlob::Import(i) => (i.use_, i.idx),
                        ImportOrGlob::Glob(i) => (i.use_, i.idx),
                    };
                    let src = u.child_source(db);
                    let tree = src.value[i].clone();
                    let pos = src
                        .file_id
                        .original_call_node(db)
                        .map_or_else(|| tree.syntax().text_range(), |n| n.value.text_range());
                    return Some(Value::Import(tree, pos));
                }
                None
            }
            _ => None,
        }
    }
}
