//! Lookup hir elements using positions in the source code. This is a lossy
//! transformation: in general, a single source might correspond to several
//! modules, functions, etc, due to macros, cfgs and `#[path=]` attributes on
//! modules.
//!
//! So, this modules should not be used during hir construction, it exists
//! purely for "IDE needs".
use std::sync::Arc;

use hir_def::{
    expr::{ExprId, PatId},
    path::known,
    resolver::{self, resolver_for_scope, HasResolver, Resolver, TypeNs, ValueNs},
    DefWithBodyId,
};
use hir_expand::{
    name::AsName, AstId, HirFileId, MacroCallId, MacroCallLoc, MacroFileKind, Source,
};
use ra_syntax::{
    ast::{self, AstNode},
    match_ast, AstPtr,
    SyntaxKind::*,
    SyntaxNode, SyntaxNodePtr, SyntaxToken, TextRange, TextUnit,
};

use crate::{
    db::HirDatabase,
    expr::{BodySourceMap, ExprScopes, ScopeId},
    ty::method_resolution::{self, implements_trait},
    Adt, AssocItem, Const, DefWithBody, Either, Enum, EnumVariant, FromSource, Function,
    GenericParam, HasBody, Local, MacroDef, Name, Path, ScopeDef, Static, Struct, Trait, Ty,
    TypeAlias,
};

fn try_get_resolver_for_node(db: &impl HirDatabase, node: Source<&SyntaxNode>) -> Option<Resolver> {
    match_ast! {
        match (node.value) {
            ast::Module(it) => {
                let src = node.with_value(it);
                Some(crate::Module::from_declaration(db, src)?.id.resolver(db))
            },
             ast::SourceFile(it) => {
                let src = node.with_value(crate::ModuleSource::SourceFile(it));
                Some(crate::Module::from_definition(db, src)?.id.resolver(db))
            },
            ast::StructDef(it) => {
                let src = node.with_value(it);
                Some(Struct::from_source(db, src)?.id.resolver(db))
            },
            ast::EnumDef(it) => {
                let src = node.with_value(it);
                Some(Enum::from_source(db, src)?.id.resolver(db))
            },
            _ => match node.value.kind() {
                FN_DEF | CONST_DEF | STATIC_DEF => {
                    let def = def_with_body_from_child_node(db, node)?;
                    let def = DefWithBodyId::from(def);
                    Some(def.resolver(db))
                }
                // FIXME add missing cases
                _ => None
            }
        }
    }
}

fn def_with_body_from_child_node(
    db: &impl HirDatabase,
    child: Source<&SyntaxNode>,
) -> Option<DefWithBody> {
    child.value.ancestors().find_map(|node| {
        match_ast! {
            match node {
                ast::FnDef(def)  => { return Function::from_source(db, child.with_value(def)).map(DefWithBody::from); },
                ast::ConstDef(def) => { return Const::from_source(db, child.with_value(def)).map(DefWithBody::from); },
                ast::StaticDef(def) => { return Static::from_source(db, child.with_value(def)).map(DefWithBody::from); },
                _ => { None },
            }
        }
    })
}

/// `SourceAnalyzer` is a convenience wrapper which exposes HIR API in terms of
/// original source files. It should not be used inside the HIR itself.
#[derive(Debug)]
pub struct SourceAnalyzer {
    file_id: HirFileId,
    resolver: Resolver,
    body_owner: Option<DefWithBody>,
    body_source_map: Option<Arc<BodySourceMap>>,
    infer: Option<Arc<crate::ty::InferenceResult>>,
    scopes: Option<Arc<crate::expr::ExprScopes>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathResolution {
    /// An item
    Def(crate::ModuleDef),
    /// A local binding (only value namespace)
    Local(Local),
    /// A generic parameter
    GenericParam(GenericParam),
    SelfType(crate::ImplBlock),
    Macro(MacroDef),
    AssocItem(crate::AssocItem),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeEntryWithSyntax {
    pub(crate) name: Name,
    pub(crate) ptr: Either<AstPtr<ast::Pat>, AstPtr<ast::SelfParam>>,
}

impl ScopeEntryWithSyntax {
    pub fn name(&self) -> &Name {
        &self.name
    }

    pub fn ptr(&self) -> Either<AstPtr<ast::Pat>, AstPtr<ast::SelfParam>> {
        self.ptr
    }
}

#[derive(Debug)]
pub struct ReferenceDescriptor {
    pub range: TextRange,
    pub name: String,
}

pub struct Expansion {
    macro_file_kind: MacroFileKind,
    macro_call_id: MacroCallId,
}

impl Expansion {
    pub fn map_token_down(
        &self,
        db: &impl HirDatabase,
        token: Source<&SyntaxToken>,
    ) -> Option<Source<SyntaxToken>> {
        let exp_info = self.file_id().expansion_info(db)?;
        exp_info.map_token_down(token)
    }

    pub fn file_id(&self) -> HirFileId {
        self.macro_call_id.as_file(self.macro_file_kind)
    }
}

impl SourceAnalyzer {
    pub fn new(
        db: &impl HirDatabase,
        node: Source<&SyntaxNode>,
        offset: Option<TextUnit>,
    ) -> SourceAnalyzer {
        let def_with_body = def_with_body_from_child_node(db, node);
        if let Some(def) = def_with_body {
            let source_map = def.body_source_map(db);
            let scopes = def.expr_scopes(db);
            let scope = match offset {
                None => scope_for(&scopes, &source_map, node),
                Some(offset) => scope_for_offset(&scopes, &source_map, node.with_value(offset)),
            };
            let resolver = resolver_for_scope(db, def.into(), scope);
            SourceAnalyzer {
                resolver,
                body_owner: Some(def),
                body_source_map: Some(source_map),
                infer: Some(def.infer(db)),
                scopes: Some(scopes),
                file_id: node.file_id,
            }
        } else {
            SourceAnalyzer {
                resolver: node
                    .value
                    .ancestors()
                    .find_map(|it| try_get_resolver_for_node(db, node.with_value(&it)))
                    .unwrap_or_default(),
                body_owner: None,
                body_source_map: None,
                infer: None,
                scopes: None,
                file_id: node.file_id,
            }
        }
    }

    fn expr_id(&self, expr: &ast::Expr) -> Option<ExprId> {
        let src = Source { file_id: self.file_id, value: expr };
        self.body_source_map.as_ref()?.node_expr(src)
    }

    fn pat_id(&self, pat: &ast::Pat) -> Option<PatId> {
        let src = Source { file_id: self.file_id, value: pat };
        self.body_source_map.as_ref()?.node_pat(src)
    }

    pub fn type_of(&self, _db: &impl HirDatabase, expr: &ast::Expr) -> Option<crate::Ty> {
        let expr_id = self.expr_id(expr)?;
        Some(self.infer.as_ref()?[expr_id].clone())
    }

    pub fn type_of_pat(&self, _db: &impl HirDatabase, pat: &ast::Pat) -> Option<crate::Ty> {
        let pat_id = self.pat_id(pat)?;
        Some(self.infer.as_ref()?[pat_id].clone())
    }

    pub fn resolve_method_call(&self, call: &ast::MethodCallExpr) -> Option<Function> {
        let expr_id = self.expr_id(&call.clone().into())?;
        self.infer.as_ref()?.method_resolution(expr_id)
    }

    pub fn resolve_field(&self, field: &ast::FieldExpr) -> Option<crate::StructField> {
        let expr_id = self.expr_id(&field.clone().into())?;
        self.infer.as_ref()?.field_resolution(expr_id)
    }

    pub fn resolve_record_field(&self, field: &ast::RecordField) -> Option<crate::StructField> {
        let expr_id = self.expr_id(&field.expr()?)?;
        self.infer.as_ref()?.record_field_resolution(expr_id)
    }

    pub fn resolve_record_literal(&self, record_lit: &ast::RecordLit) -> Option<crate::VariantDef> {
        let expr_id = self.expr_id(&record_lit.clone().into())?;
        self.infer.as_ref()?.variant_resolution_for_expr(expr_id)
    }

    pub fn resolve_record_pattern(&self, record_pat: &ast::RecordPat) -> Option<crate::VariantDef> {
        let pat_id = self.pat_id(&record_pat.clone().into())?;
        self.infer.as_ref()?.variant_resolution_for_pat(pat_id)
    }

    pub fn resolve_macro_call(
        &self,
        db: &impl HirDatabase,
        macro_call: &ast::MacroCall,
    ) -> Option<MacroDef> {
        // This must be a normal source file rather than macro file.
        let path = macro_call.path().and_then(Path::from_ast)?;
        self.resolver.resolve_path_as_macro(db, &path).map(|it| it.into())
    }

    pub fn resolve_hir_path(
        &self,
        db: &impl HirDatabase,
        path: &crate::Path,
    ) -> Option<PathResolution> {
        let types = self.resolver.resolve_path_in_type_ns_fully(db, &path).map(|ty| match ty {
            TypeNs::SelfType(it) => PathResolution::SelfType(it.into()),
            TypeNs::GenericParam(idx) => PathResolution::GenericParam(GenericParam {
                parent: self.resolver.generic_def().unwrap().into(),
                idx,
            }),
            TypeNs::AdtSelfType(it) | TypeNs::AdtId(it) => {
                PathResolution::Def(Adt::from(it).into())
            }
            TypeNs::EnumVariantId(it) => PathResolution::Def(EnumVariant::from(it).into()),
            TypeNs::TypeAliasId(it) => PathResolution::Def(TypeAlias::from(it).into()),
            TypeNs::BuiltinType(it) => PathResolution::Def(it.into()),
            TypeNs::TraitId(it) => PathResolution::Def(Trait::from(it).into()),
        });
        let values = self.resolver.resolve_path_in_value_ns_fully(db, &path).and_then(|val| {
            let res = match val {
                ValueNs::LocalBinding(pat_id) => {
                    let var = Local { parent: self.body_owner?, pat_id };
                    PathResolution::Local(var)
                }
                ValueNs::FunctionId(it) => PathResolution::Def(Function::from(it).into()),
                ValueNs::ConstId(it) => PathResolution::Def(Const::from(it).into()),
                ValueNs::StaticId(it) => PathResolution::Def(Static::from(it).into()),
                ValueNs::StructId(it) => PathResolution::Def(Struct::from(it).into()),
                ValueNs::EnumVariantId(it) => PathResolution::Def(EnumVariant::from(it).into()),
            };
            Some(res)
        });

        let items = self
            .resolver
            .resolve_module_path(db, &path)
            .take_types()
            .map(|it| PathResolution::Def(it.into()));
        types.or(values).or(items).or_else(|| {
            self.resolver
                .resolve_path_as_macro(db, &path)
                .map(|def| PathResolution::Macro(def.into()))
        })
    }

    pub fn resolve_path(&self, db: &impl HirDatabase, path: &ast::Path) -> Option<PathResolution> {
        if let Some(path_expr) = path.syntax().parent().and_then(ast::PathExpr::cast) {
            let expr_id = self.expr_id(&path_expr.into())?;
            if let Some(assoc) = self.infer.as_ref()?.assoc_resolutions_for_expr(expr_id) {
                return Some(PathResolution::AssocItem(assoc));
            }
        }
        if let Some(path_pat) = path.syntax().parent().and_then(ast::PathPat::cast) {
            let pat_id = self.pat_id(&path_pat.into())?;
            if let Some(assoc) = self.infer.as_ref()?.assoc_resolutions_for_pat(pat_id) {
                return Some(PathResolution::AssocItem(assoc));
            }
        }
        // This must be a normal source file rather than macro file.
        let hir_path = crate::Path::from_ast(path.clone())?;
        self.resolve_hir_path(db, &hir_path)
    }

    fn resolve_local_name(&self, name_ref: &ast::NameRef) -> Option<ScopeEntryWithSyntax> {
        let name = name_ref.as_name();
        let source_map = self.body_source_map.as_ref()?;
        let scopes = self.scopes.as_ref()?;
        let scope = scope_for(scopes, source_map, Source::new(self.file_id, name_ref.syntax()))?;
        let entry = scopes.resolve_name_in_scope(scope, &name)?;
        Some(ScopeEntryWithSyntax {
            name: entry.name().clone(),
            ptr: source_map.pat_syntax(entry.pat())?.value,
        })
    }

    pub fn process_all_names(&self, db: &impl HirDatabase, f: &mut dyn FnMut(Name, ScopeDef)) {
        self.resolver.process_all_names(db, &mut |name, def| {
            let def = match def {
                resolver::ScopeDef::PerNs(it) => it.into(),
                resolver::ScopeDef::ImplSelfType(it) => ScopeDef::ImplSelfType(it.into()),
                resolver::ScopeDef::AdtSelfType(it) => ScopeDef::AdtSelfType(it.into()),
                resolver::ScopeDef::GenericParam(idx) => {
                    let parent = self.resolver.generic_def().unwrap().into();
                    ScopeDef::GenericParam(GenericParam { parent, idx })
                }
                resolver::ScopeDef::Local(pat_id) => {
                    let parent = self.resolver.body_owner().unwrap().into();
                    ScopeDef::Local(Local { parent, pat_id })
                }
            };
            f(name, def)
        })
    }

    // FIXME: we only use this in `inline_local_variable` assist, ideally, we
    // should switch to general reference search infra there.
    pub fn find_all_refs(&self, pat: &ast::BindPat) -> Vec<ReferenceDescriptor> {
        let fn_def = pat.syntax().ancestors().find_map(ast::FnDef::cast).unwrap();
        let ptr = Either::A(AstPtr::new(&ast::Pat::from(pat.clone())));
        fn_def
            .syntax()
            .descendants()
            .filter_map(ast::NameRef::cast)
            .filter(|name_ref| match self.resolve_local_name(&name_ref) {
                None => false,
                Some(entry) => entry.ptr() == ptr,
            })
            .map(|name_ref| ReferenceDescriptor {
                name: name_ref.text().to_string(),
                range: name_ref.syntax().text_range(),
            })
            .collect()
    }

    pub fn iterate_method_candidates<T>(
        &self,
        db: &impl HirDatabase,
        ty: Ty,
        name: Option<&Name>,
        mut callback: impl FnMut(&Ty, Function) -> Option<T>,
    ) -> Option<T> {
        // There should be no inference vars in types passed here
        // FIXME check that?
        // FIXME replace Unknown by bound vars here
        let canonical = crate::ty::Canonical { value: ty, num_vars: 0 };
        method_resolution::iterate_method_candidates(
            &canonical,
            db,
            &self.resolver,
            name,
            method_resolution::LookupMode::MethodCall,
            |ty, it| match it {
                AssocItem::Function(f) => callback(ty, f),
                _ => None,
            },
        )
    }

    pub fn iterate_path_candidates<T>(
        &self,
        db: &impl HirDatabase,
        ty: Ty,
        name: Option<&Name>,
        callback: impl FnMut(&Ty, AssocItem) -> Option<T>,
    ) -> Option<T> {
        // There should be no inference vars in types passed here
        // FIXME check that?
        // FIXME replace Unknown by bound vars here
        let canonical = crate::ty::Canonical { value: ty, num_vars: 0 };
        method_resolution::iterate_method_candidates(
            &canonical,
            db,
            &self.resolver,
            name,
            method_resolution::LookupMode::Path,
            callback,
        )
    }

    pub fn autoderef<'a>(
        &'a self,
        db: &'a impl HirDatabase,
        ty: Ty,
    ) -> impl Iterator<Item = Ty> + 'a {
        // There should be no inference vars in types passed here
        // FIXME check that?
        let canonical = crate::ty::Canonical { value: ty, num_vars: 0 };
        crate::ty::autoderef(db, &self.resolver, canonical).map(|canonical| canonical.value)
    }

    /// Checks that particular type `ty` implements `std::future::Future`.
    /// This function is used in `.await` syntax completion.
    pub fn impls_future(&self, db: &impl HirDatabase, ty: Ty) -> bool {
        let std_future_path = known::std_future_future();

        let std_future_trait = match self.resolver.resolve_known_trait(db, &std_future_path) {
            Some(it) => it.into(),
            _ => return false,
        };

        let krate = match self.resolver.krate() {
            Some(krate) => krate,
            _ => return false,
        };

        let canonical_ty = crate::ty::Canonical { value: ty, num_vars: 0 };
        implements_trait(&canonical_ty, db, &self.resolver, krate.into(), std_future_trait)
    }

    pub fn expand(
        &self,
        db: &impl HirDatabase,
        macro_call: Source<&ast::MacroCall>,
    ) -> Option<Expansion> {
        let def = self.resolve_macro_call(db, macro_call.value)?.id;
        let ast_id = AstId::new(
            macro_call.file_id,
            db.ast_id_map(macro_call.file_id).ast_id(macro_call.value),
        );
        let macro_call_loc = MacroCallLoc { def, ast_id };
        Some(Expansion {
            macro_call_id: db.intern_macro(macro_call_loc),
            macro_file_kind: to_macro_file_kind(macro_call.value),
        })
    }

    #[cfg(test)]
    pub(crate) fn body_source_map(&self) -> Arc<BodySourceMap> {
        self.body_source_map.clone().unwrap()
    }

    #[cfg(test)]
    pub(crate) fn inference_result(&self) -> Arc<crate::ty::InferenceResult> {
        self.infer.clone().unwrap()
    }

    #[cfg(test)]
    pub(crate) fn analyzed_declaration(&self) -> Option<DefWithBody> {
        self.body_owner
    }
}

fn scope_for(
    scopes: &ExprScopes,
    source_map: &BodySourceMap,
    node: Source<&SyntaxNode>,
) -> Option<ScopeId> {
    node.value
        .ancestors()
        .filter_map(ast::Expr::cast)
        .filter_map(|it| source_map.node_expr(Source::new(node.file_id, &it)))
        .find_map(|it| scopes.scope_for(it))
}

fn scope_for_offset(
    scopes: &ExprScopes,
    source_map: &BodySourceMap,
    offset: Source<TextUnit>,
) -> Option<ScopeId> {
    scopes
        .scope_by_expr()
        .iter()
        .filter_map(|(id, scope)| {
            let source = source_map.expr_syntax(*id)?;
            // FIXME: correctly handle macro expansion
            if source.file_id != offset.file_id {
                return None;
            }
            let syntax_node_ptr =
                source.value.either(|it| it.syntax_node_ptr(), |it| it.syntax_node_ptr());
            Some((syntax_node_ptr, scope))
        })
        // find containing scope
        .min_by_key(|(ptr, _scope)| {
            (
                !(ptr.range().start() <= offset.value && offset.value <= ptr.range().end()),
                ptr.range().len(),
            )
        })
        .map(|(ptr, scope)| {
            adjust(scopes, source_map, ptr, offset.file_id, offset.value).unwrap_or(*scope)
        })
}

// XXX: during completion, cursor might be outside of any particular
// expression. Try to figure out the correct scope...
fn adjust(
    scopes: &ExprScopes,
    source_map: &BodySourceMap,
    ptr: SyntaxNodePtr,
    file_id: HirFileId,
    offset: TextUnit,
) -> Option<ScopeId> {
    let r = ptr.range();
    let child_scopes = scopes
        .scope_by_expr()
        .iter()
        .filter_map(|(id, scope)| {
            let source = source_map.expr_syntax(*id)?;
            // FIXME: correctly handle macro expansion
            if source.file_id != file_id {
                return None;
            }
            let syntax_node_ptr =
                source.value.either(|it| it.syntax_node_ptr(), |it| it.syntax_node_ptr());
            Some((syntax_node_ptr, scope))
        })
        .map(|(ptr, scope)| (ptr.range(), scope))
        .filter(|(range, _)| range.start() <= offset && range.is_subrange(&r) && *range != r);

    child_scopes
        .max_by(|(r1, _), (r2, _)| {
            if r2.is_subrange(&r1) {
                std::cmp::Ordering::Greater
            } else if r1.is_subrange(&r2) {
                std::cmp::Ordering::Less
            } else {
                r1.start().cmp(&r2.start())
            }
        })
        .map(|(_ptr, scope)| *scope)
}

/// Given a `ast::MacroCall`, return what `MacroKindFile` it belongs to.
/// FIXME: Not completed
fn to_macro_file_kind(macro_call: &ast::MacroCall) -> MacroFileKind {
    let syn = macro_call.syntax();
    let parent = match syn.parent() {
        Some(it) => it,
        None => {
            // FIXME:
            // If it is root, which means the parent HirFile
            // MacroKindFile must be non-items
            // return expr now.
            return MacroFileKind::Expr;
        }
    };

    match parent.kind() {
        MACRO_ITEMS | SOURCE_FILE => MacroFileKind::Items,
        LET_STMT => {
            // FIXME: Handle Pattern
            MacroFileKind::Expr
        }
        EXPR_STMT => MacroFileKind::Statements,
        BLOCK => MacroFileKind::Statements,
        ARG_LIST => MacroFileKind::Expr,
        TRY_EXPR => MacroFileKind::Expr,
        _ => {
            // Unknown , Just guess it is `Items`
            MacroFileKind::Items
        }
    }
}
