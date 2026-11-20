use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::path::Path;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

use crate::pull::{self, MatchingSeed, RustItemKind, UnitKey, is_call_invocation};
use crate::units::{self, CallEdge};

pub const MAX_DEPTH: usize = 5;
pub const MAX_ADDED_UNITS: usize = 25;
pub const MAX_ADDED_CHARS: usize = 24_000;
pub const MAX_UNIT_LINES: usize = 200;
pub const UTILITY_THRESHOLD: usize = 8;
pub const REPORT_RESERVE_CHARS: usize = 1_500;

pub fn extract_directive(args: &[String]) -> Result<(Option<usize>, Vec<String>), String> {
    let mut cascade_depth = None;
    let mut remaining = Vec::new();

    for arg in args {
        if arg.starts_with("-cascade:") {
            return Err(format!(
                "CVB pull: negated selector '{arg}' is not supported; exclusions only apply to content terms."
            ));
        }

        if let Some(rest) = arg.strip_prefix("cascade:") {
            if cascade_depth.is_some() {
                return Err("CVB pull: only one cascade: directive is allowed.".to_string());
            }
            if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
                return Err(
                    "CVB pull: cascade: must be followed by a depth from 0 to 5.".to_string(),
                );
            }
            let depth: usize = rest.parse().map_err(|_| {
                "CVB pull: cascade: must be followed by a depth from 0 to 5.".to_string()
            })?;
            if depth > MAX_DEPTH {
                return Err(
                    "CVB pull: cascade: must be followed by a depth from 0 to 5.".to_string(),
                );
            }
            cascade_depth = Some(depth);
        } else {
            remaining.push(arg.clone());
        }
    }

    if cascade_depth.is_some() {
        if remaining.iter().any(|arg| arg.starts_with("callers:")) {
            return Err("CVB pull: cascade: cannot be combined with callers:.".to_string());
        }
        if remaining.iter().any(|arg| arg.starts_with("lines:")) {
            return Err("CVB pull: cascade: cannot be combined with lines:.".to_string());
        }
        if remaining.len() == 1 && remaining[0].starts_with("file:") {
            return Err(
                "CVB pull: cascade: cannot be used with whole-file capture; provide a search term to select initial seeds.".to_string(),
            );
        }
    }

    Ok((cascade_depth, remaining))
}

#[derive(Debug, Clone)]
pub struct IndexedDef {
    pub path: String,
    pub label: &'static str,
    pub name: String,
    #[allow(dead_code)]
    pub owner: Option<String>,
    pub start: usize,
    pub end: usize,
    #[allow(dead_code)]
    pub param_types: Vec<(String, String)>,
}

impl IndexedDef {
    pub fn unit_key(&self) -> UnitKey {
        UnitKey {
            path: self.path.clone(),
            start: self.start,
            end: self.end,
        }
    }
}

pub struct Index {
    pub free_fns: HashMap<String, Vec<IndexedDef>>,
    pub methods: HashMap<(String, String), Vec<IndexedDef>>,
    pub types: HashMap<String, Vec<IndexedDef>>,
    #[allow(dead_code)]
    pub traits: HashMap<String, Vec<IndexedDef>>,
    pub file_uses: HashMap<String, HashMap<String, String>>,
    pub file_sources: HashMap<String, String>,
    pub parse_warnings: Vec<String>,
    pub total_callers_count: HashMap<UnitKey, usize>,
    /// Functions and calls of the tree-sitter languages (everything but Rust).
    pub generic: GenericIndex,
}

/// The index of the languages `units` reads. Kept apart from the Rust index so
/// Rust results cannot change when a repository also holds other languages.
#[derive(Default)]
pub struct GenericIndex {
    /// (language, bare name) to the function definitions of that name, test
    /// files excluded.
    pub fns: HashMap<(usize, String), Vec<IndexedDef>>,
    /// Per file: its language and its calls.
    pub calls: HashMap<String, (usize, Vec<CallEdge>)>,
    /// Per file: its function definitions, to name the unit around a call.
    pub defs: HashMap<String, Vec<units::Definition>>,
    pub sources: HashMap<String, String>,
}

impl Index {
    /// The text of an indexed file of any language.
    fn source(&self, path: &str) -> Option<&String> {
        self.file_sources
            .get(path)
            .or_else(|| self.generic.sources.get(path))
    }
}

/// The identity of a language, for keeping same-named functions of different
/// languages apart.
fn language_id(lang: &units::Lang) -> usize {
    lang as *const units::Lang as usize
}

/// Test code is not followed: files named or placed like tests.
fn is_test_path(path: &str) -> bool {
    let path = Path::new(path);
    let in_test_dir = path.components().any(|part| {
        matches!(
            part.as_os_str().to_str(),
            Some("test" | "tests" | "__tests__" | "spec" | "specs")
        )
    });
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    in_test_dir
        || stem.starts_with("test_")
        || ["_test", ".test", ".spec", "_spec", "Test", "Tests"]
            .iter()
            .any(|suffix| stem.ends_with(suffix))
}

fn add_generic_files(index: &mut Index, files: &[String], dir: Option<&Path>) {
    for path in files {
        if Path::new(path).extension() == Some(OsStr::new("rs")) || is_test_path(path) {
            continue;
        }
        let Some(lang) = units::callers_language_for(path) else {
            continue;
        };
        let full_path = dir.map_or_else(|| Path::new(path).to_path_buf(), |d| d.join(path));
        let source = match std::fs::read_to_string(&full_path) {
            Ok(source) => source,
            Err(error) => {
                index
                    .parse_warnings
                    .push(format!("skipping unreadable file {path}: {error}; index incomplete."));
                continue;
            }
        };
        let Some(outline) = units::outline(lang, &source) else {
            index
                .parse_warnings
                .push(format!("skipping unparseable file {path}; index incomplete."));
            continue;
        };
        if outline.has_errors {
            index.parse_warnings.push(format!(
                "{path} has syntax errors; functions in the broken parts are missing; index incomplete."
            ));
        }
        let id = language_id(lang);
        for def in &outline.definitions {
            index
                .generic
                .fns
                .entry((id, def.bare_name().to_string()))
                .or_default()
                .push(IndexedDef {
                    path: path.clone(),
                    label: def.label,
                    name: def.bare_name().to_string(),
                    owner: None,
                    start: def.start,
                    end: def.end,
                    param_types: Vec::new(),
                });
        }
        index.generic.defs.insert(path.clone(), outline.definitions);
        index.generic.calls.insert(path.clone(), (id, outline.calls));
        index.generic.sources.insert(path.clone(), source);
    }

    // How many distinct units call each function, to recognise utilities.
    let mut callers: HashMap<UnitKey, HashSet<UnitKey>> = HashMap::new();
    for (path, (id, calls)) in &index.generic.calls {
        for call in calls {
            let Some((start, end)) = call.within else {
                continue;
            };
            if call.member && !is_own_receiver(call.receiver.as_deref()) {
                continue;
            }
            if let Some(defs) = index.generic.fns.get(&(*id, call.name.clone()))
                && defs.len() == 1
            {
                callers
                    .entry(defs[0].unit_key())
                    .or_default()
                    .insert(UnitKey { path: path.clone(), start, end });
            }
        }
    }
    for (key, set) in callers {
        index.total_callers_count.insert(key, set.len());
    }
}

fn is_own_receiver(receiver: Option<&str>) -> bool {
    matches!(receiver, Some("self" | "this" | "cls"))
}

fn extract_type_name_from_type(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(tp) => {
            let last_seg = tp.path.segments.last()?;
            let ident_str = last_seg.ident.to_string();
            if matches!(ident_str.as_str(), "Box" | "Arc" | "Rc")
                && let syn::PathArguments::AngleBracketed(ab) = &last_seg.arguments
                    && let Some(syn::GenericArgument::Type(inner)) = ab.args.first() {
                        return extract_type_name_from_type(inner);
                    }
            Some(ident_str)
        }
        syn::Type::Reference(tr) => extract_type_name_from_type(&tr.elem),
        _ => None,
    }
}

fn collect_type_names(ty: &syn::Type, out: &mut Vec<String>) {
    match ty {
        syn::Type::Path(tp) => {
            for seg in &tp.path.segments {
                let s = seg.ident.to_string();
                if s.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    out.push(s);
                }
                if let syn::PathArguments::AngleBracketed(ab) = &seg.arguments {
                    for arg in &ab.args {
                        if let syn::GenericArgument::Type(inner) = arg {
                            collect_type_names(inner, out);
                        }
                    }
                }
            }
        }
        syn::Type::Reference(tr) => {
            collect_type_names(&tr.elem, out);
        }
        syn::Type::Slice(ts) => {
            collect_type_names(&ts.elem, out);
        }
        syn::Type::Tuple(tt) => {
            for elem in &tt.elems {
                collect_type_names(elem, out);
            }
        }
        _ => {}
    }
}

fn extract_fn_params(sig: &syn::Signature) -> Vec<(String, String)> {
    let mut params = Vec::new();
    for input in &sig.inputs {
        if let syn::FnArg::Typed(pat_type) = input
            && let syn::Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
                let name = pat_ident.ident.to_string();
                let ty_name = extract_type_name_from_type(&pat_type.ty).unwrap_or_default();
                params.push((name, ty_name));
            }
    }
    params
}

fn collect_use_tree(tree: &syn::UseTree, prefix: &str, out: &mut HashMap<String, String>) {
    match tree {
        syn::UseTree::Path(p) => {
            let next_prefix = if prefix.is_empty() {
                p.ident.to_string()
            } else {
                format!("{prefix}::{}", p.ident)
            };
            collect_use_tree(&p.tree, &next_prefix, out);
        }
        syn::UseTree::Name(n) => {
            let ident = n.ident.to_string();
            let full_path = if prefix.is_empty() {
                ident.clone()
            } else {
                format!("{prefix}::{ident}")
            };
            out.insert(ident, full_path);
        }
        syn::UseTree::Rename(r) => {
            let rename = r.rename.to_string();
            let full_path = if prefix.is_empty() {
                r.ident.to_string()
            } else {
                format!("{prefix}::{}", r.ident)
            };
            out.insert(rename, full_path);
        }
        syn::UseTree::Group(g) => {
            for item in &g.items {
                collect_use_tree(item, prefix, out);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

pub fn build_index(dir: Option<&Path>) -> Index {
    let mut free_fns: HashMap<String, Vec<IndexedDef>> = HashMap::new();
    let mut methods: HashMap<(String, String), Vec<IndexedDef>> = HashMap::new();
    let mut types: HashMap<String, Vec<IndexedDef>> = HashMap::new();
    let mut traits: HashMap<String, Vec<IndexedDef>> = HashMap::new();
    let mut file_uses: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut file_sources: HashMap<String, String> = HashMap::new();
    let mut parse_warnings: Vec<String> = Vec::new();

    let tracked = match pull::tracked_files(dir) {
        Ok(files) => files,
        Err(_) => {
            return Index {
                free_fns,
                methods,
                types,
                traits,
                file_uses,
                file_sources,
                parse_warnings,
                total_callers_count: HashMap::new(),
                generic: GenericIndex::default(),
            };
        }
    };

    let all_files: Vec<String> = tracked
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    let rs_files: Vec<String> = tracked
        .into_iter()
        .filter(|p| Path::new(p).extension() == Some(OsStr::new("rs")))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    let mut parsed_files = Vec::new();

    for path in rs_files {
        let full_path = if let Some(d) = dir {
            d.join(&path)
        } else {
            Path::new(&path).to_path_buf()
        };

        let source = match std::fs::read_to_string(&full_path) {
            Ok(src) => src,
            Err(e) => {
                parse_warnings.push(format!(
                    "skipping unreadable Rust file {path}: {e}; index incomplete."
                ));
                continue;
            }
        };

        let file = match syn::parse_file(&source) {
            Ok(f) => f,
            Err(_) => {
                parse_warnings.push(format!(
                    "skipping unparseable Rust file {path}; index incomplete."
                ));
                continue;
            }
        };

        let mut uses = HashMap::new();
        for item in &file.items {
            if let syn::Item::Use(u) = item {
                collect_use_tree(&u.tree, "", &mut uses);
            }
        }
        file_uses.insert(path.clone(), uses);

        let items = pull::rust_items(&file);

        let mut fn_params_map: HashMap<(String, usize), Vec<(String, String)>> = HashMap::new();
        struct FnParamVisitor<'a> {
            params_map: &'a mut HashMap<(String, usize), Vec<(String, String)>>,
        }
        impl<'ast> Visit<'ast> for FnParamVisitor<'_> {
            fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
                let name = node.sig.ident.to_string();
                let line = node.sig.span().start().line;
                self.params_map
                    .insert((name, line), extract_fn_params(&node.sig));
                visit::visit_item_fn(self, node);
            }
            fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
                let name = node.sig.ident.to_string();
                let line = node.sig.span().start().line;
                self.params_map
                    .insert((name, line), extract_fn_params(&node.sig));
                visit::visit_impl_item_fn(self, node);
            }
            fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
                let name = node.sig.ident.to_string();
                let line = node.sig.span().start().line;
                self.params_map
                    .insert((name, line), extract_fn_params(&node.sig));
                visit::visit_trait_item_fn(self, node);
            }
        }
        let mut visitor = FnParamVisitor {
            params_map: &mut fn_params_map,
        };
        visitor.visit_file(&file);

        for item in &items {
            if item.in_test_mod {
                continue;
            }

            match item.kind {
                RustItemKind::Function => {
                    let name = item.name.clone().unwrap_or_default();
                    let params = fn_params_map
                        .remove(&(name.clone(), item.start))
                        .unwrap_or_default();

                    let def = IndexedDef {
                        path: path.clone(),
                        label: item.kind.label(),
                        name: name.clone(),
                        owner: item.owner.clone(),
                        start: item.start,
                        end: item.end,
                        param_types: params,
                    };

                    if let Some(owner) = &item.owner {
                        methods.entry((owner.clone(), name)).or_default().push(def);
                    } else {
                        free_fns.entry(name).or_default().push(def);
                    }
                }
                RustItemKind::Struct | RustItemKind::Enum => {
                    let name = item.name.clone().unwrap_or_default();
                    let def = IndexedDef {
                        path: path.clone(),
                        label: item.kind.label(),
                        name: name.clone(),
                        owner: None,
                        start: item.start,
                        end: item.end,
                        param_types: Vec::new(),
                    };
                    types.entry(name).or_default().push(def);
                }
                RustItemKind::Trait => {
                    let name = item.name.clone().unwrap_or_default();
                    let def = IndexedDef {
                        path: path.clone(),
                        label: item.kind.label(),
                        name: name.clone(),
                        owner: None,
                        start: item.start,
                        end: item.end,
                        param_types: Vec::new(),
                    };
                    traits.entry(name).or_default().push(def);
                }
                RustItemKind::Impl => {}
            }
        }

        parsed_files.push((path.clone(), file));
        file_sources.insert(path, source);
    }

    let mut total_callers_count = HashMap::new();

    // Collect all valid indexed unit keys to ensure calls are attributed only to real units
    let mut known_units: HashSet<UnitKey> = HashSet::new();
    for defs in free_fns.values() {
        for d in defs {
            known_units.insert(d.unit_key());
        }
    }
    for defs in methods.values() {
        for d in defs {
            known_units.insert(d.unit_key());
        }
    }

    struct CallTallyVisitor<'a> {
        path: &'a str,
        free_fns: &'a HashMap<String, Vec<IndexedDef>>,
        current_unit: Option<UnitKey>,
        tally: &'a mut HashMap<UnitKey, HashSet<UnitKey>>,
        known_units: &'a HashSet<UnitKey>,
    }

    impl<'ast> Visit<'ast> for CallTallyVisitor<'_> {
        fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
            let span = node.span();
            let key = UnitKey {
                path: self.path.to_string(),
                start: span.start().line,
                end: span.end().line,
            };
            let is_known = self.known_units.contains(&key);
            let prev = if is_known {
                self.current_unit.replace(key)
            } else {
                self.current_unit.clone()
            };
            visit::visit_item_fn(self, node);
            self.current_unit = prev;
        }

        fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
            let span = node.span();
            let key = UnitKey {
                path: self.path.to_string(),
                start: span.start().line,
                end: span.end().line,
            };
            let is_known = self.known_units.contains(&key);
            let prev = if is_known {
                self.current_unit.replace(key)
            } else {
                self.current_unit.clone()
            };
            visit::visit_impl_item_fn(self, node);
            self.current_unit = prev;
        }

        fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
            let span = node.span();
            let key = UnitKey {
                path: self.path.to_string(),
                start: span.start().line,
                end: span.end().line,
            };
            let is_known = self.known_units.contains(&key);
            let prev = if is_known {
                self.current_unit.replace(key)
            } else {
                self.current_unit.clone()
            };
            visit::visit_trait_item_fn(self, node);
            self.current_unit = prev;
        }

        fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
            if let syn::Expr::Path(p) = node.func.as_ref()
                && let Some(ident) = p.path.segments.last().map(|s| s.ident.to_string())
                    && let Some(defs) = self.free_fns.get(&ident)
                        && defs.len() == 1
                            && let Some(cur) = &self.current_unit {
                                self.tally
                                    .entry(defs[0].unit_key())
                                    .or_default()
                                    .insert(cur.clone());
                            }
            visit::visit_expr_call(self, node);
        }
    }

    let mut caller_sets = HashMap::new();
    for (path, file) in &parsed_files {
        let mut visitor = CallTallyVisitor {
            path,
            free_fns: &free_fns,
            current_unit: None,
            tally: &mut caller_sets,
            known_units: &known_units,
        };
        visitor.visit_file(file);
    }

    for (k, set) in caller_sets {
        total_callers_count.insert(k, set.len());
    }

    let mut index = Index {
        free_fns,
        methods,
        types,
        traits,
        file_uses,
        file_sources,
        parse_warnings,
        total_callers_count,
        generic: GenericIndex::default(),
    };
    add_generic_files(&mut index, &all_files, dir);
    index
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeKind {
    PathCall {
        qualifier: Option<String>,
        name: String,
        line: usize,
    },
    SelfCall {
        owner: Option<String>,
        name: String,
        line: usize,
    },
    MethodCallTyped {
        receiver_type: String,
        method: String,
        line: usize,
        source_var: String,
    },
    MethodCallUnknown {
        method: String,
        line: usize,
    },
    MacroCall {
        name: String,
        line: usize,
    },
    FnAsValue {
        name: String,
        line: usize,
    },
    SignatureType {
        name: String,
    },
}

struct UnitEdgeVisitor {
    edges: Vec<EdgeKind>,
    fn_params: HashMap<String, String>,
    local_vars: HashMap<String, String>,
    enclosing_owner: Option<String>,
}

impl<'ast> Visit<'ast> for UnitEdgeVisitor {
    fn visit_local(&mut self, node: &'ast syn::Local) {
        if let syn::Pat::Type(pat_type) = &node.pat {
            if let syn::Pat::Ident(pat_ident) = pat_type.pat.as_ref()
                && let Some(t) = extract_type_name_from_type(&pat_type.ty) {
                    self.local_vars.insert(pat_ident.ident.to_string(), t);
                }
        } else if let Some(init) = &node.init
            && let syn::Expr::Call(call) = init.expr.as_ref()
                && let syn::Expr::Path(p) = call.func.as_ref()
                    && p.path.segments.len() >= 2 {
                        let seg0 = p.path.segments[0].ident.to_string();
                        if seg0.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                            && let syn::Pat::Ident(pat_ident) = &node.pat {
                                self.local_vars.insert(pat_ident.ident.to_string(), seg0);
                            }
                    }
        visit::visit_local(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        let line = node.span().start().line;
        if let syn::Expr::Path(p) = node.func.as_ref() {
            if p.path.segments.len() == 1 {
                let name = p.path.segments[0].ident.to_string();
                if self.fn_params.contains_key(&name) {
                    self.edges.push(EdgeKind::PathCall {
                        qualifier: Some("__callback__".to_string()),
                        name,
                        line,
                    });
                } else {
                    self.edges.push(EdgeKind::PathCall {
                        qualifier: None,
                        name,
                        line,
                    });
                }
            } else if p.path.segments.len() >= 2 {
                let first = p.path.segments[0].ident.to_string();
                let last = p.path.segments.last().unwrap().ident.to_string();
                if first == "Self" {
                    self.edges.push(EdgeKind::SelfCall {
                        owner: self.enclosing_owner.clone(),
                        name: last,
                        line,
                    });
                } else {
                    self.edges.push(EdgeKind::PathCall {
                        qualifier: Some(first),
                        name: last,
                        line,
                    });
                }
            }
        }

        for arg in &node.args {
            if let syn::Expr::Path(p) = arg
                && p.path.segments.len() == 1 {
                    let name = p.path.segments[0].ident.to_string();
                    if !self.fn_params.contains_key(&name) && !self.local_vars.contains_key(&name)
                        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
                            self.edges.push(EdgeKind::FnAsValue { name, line });
                        }
                }
        }

        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let line = node.span().start().line;
        let method = node.method.to_string();

        let receiver_known = match node.receiver.as_ref() {
            syn::Expr::Path(p) => {
                if p.path.is_ident("self") {
                    self.enclosing_owner
                        .as_ref()
                        .map(|owner| (owner.clone(), "self".to_string()))
                } else if let Some(ident) = p.path.get_ident() {
                    let var_name = ident.to_string();
                    if let Some(ty) = self.local_vars.get(&var_name) {
                        Some((ty.clone(), var_name))
                    } else if let Some(ty) = self.fn_params.get(&var_name) {
                        if !ty.is_empty() {
                            Some((ty.clone(), format!("parameter `{var_name}: &{ty}`")))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some((receiver_type, source_var)) = receiver_known {
            self.edges.push(EdgeKind::MethodCallTyped {
                receiver_type,
                method,
                line,
                source_var,
            });
        } else {
            self.edges
                .push(EdgeKind::MethodCallUnknown { method, line });
        }

        for arg in &node.args {
            if let syn::Expr::Path(p) = arg
                && p.path.segments.len() == 1 {
                    let name = p.path.segments[0].ident.to_string();
                    if !self.fn_params.contains_key(&name) && !self.local_vars.contains_key(&name)
                        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
                            self.edges.push(EdgeKind::FnAsValue { name, line });
                        }
                }
        }

        visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        use proc_macro2::TokenTree;
        fn scan(tokens: proc_macro2::TokenStream, edges: &mut Vec<EdgeKind>) {
            let trees: Vec<TokenTree> = tokens.into_iter().collect();
            for i in 0..trees.len() {
                match &trees[i] {
                    TokenTree::Ident(ident) => {
                        if is_call_invocation(&trees, i) {
                            edges.push(EdgeKind::MacroCall {
                                name: ident.to_string(),
                                line: ident.span().start().line,
                            });
                        }
                    }
                    TokenTree::Group(g) => scan(g.stream(), edges),
                    _ => {}
                }
            }
        }
        scan(node.tokens.clone(), &mut self.edges);
        visit::visit_macro(self, node);
    }
}

pub fn extract_unit_edges(
    source: &str,
    key: &UnitKey,
    index: &Index,
) -> (Option<String>, Vec<EdgeKind>) {
    let Ok(file) = syn::parse_file(source) else {
        return (None, Vec::new());
    };

    let items = pull::rust_items(&file);
    let item = items
        .into_iter()
        .find(|it| it.start == key.start && it.end == key.end);
    let item_name = item.as_ref().and_then(|it| it.name.clone());
    let item_owner = item.as_ref().and_then(|it| it.owner.clone());

    let mut edges = Vec::new();

    struct ExtractVisitor<'a> {
        target_start: usize,
        target_end: usize,
        edges: &'a mut Vec<EdgeKind>,
        item_owner: Option<String>,
        #[allow(dead_code)]
        index: &'a Index,
    }

    impl<'ast> Visit<'ast> for ExtractVisitor<'_> {
        fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
            let span = node.span();
            if span.start().line == self.target_start && span.end().line == self.target_end {
                for input in &node.sig.inputs {
                    if let syn::FnArg::Typed(pat_type) = input {
                        let mut types = Vec::new();
                        collect_type_names(&pat_type.ty, &mut types);
                        for t in types {
                            self.edges.push(EdgeKind::SignatureType { name: t });
                        }
                    }
                }
                if let syn::ReturnType::Type(_, ty) = &node.sig.output {
                    let mut types = Vec::new();
                    collect_type_names(ty, &mut types);
                    for t in types {
                        self.edges.push(EdgeKind::SignatureType { name: t });
                    }
                }

                let params = extract_fn_params(&node.sig);
                let fn_params = params.into_iter().collect();
                let mut v = UnitEdgeVisitor {
                    edges: Vec::new(),
                    fn_params,
                    local_vars: HashMap::new(),
                    enclosing_owner: self.item_owner.clone(),
                };
                v.visit_block(&node.block);
                self.edges.extend(v.edges);
                return;
            }
            visit::visit_item_fn(self, node);
        }

        fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
            let span = node.span();
            if span.start().line == self.target_start && span.end().line == self.target_end {
                for input in &node.sig.inputs {
                    if let syn::FnArg::Typed(pat_type) = input {
                        let mut types = Vec::new();
                        collect_type_names(&pat_type.ty, &mut types);
                        for t in types {
                            self.edges.push(EdgeKind::SignatureType { name: t });
                        }
                    }
                }
                if let syn::ReturnType::Type(_, ty) = &node.sig.output {
                    let mut types = Vec::new();
                    collect_type_names(ty, &mut types);
                    for t in types {
                        self.edges.push(EdgeKind::SignatureType { name: t });
                    }
                }

                let params = extract_fn_params(&node.sig);
                let fn_params = params.into_iter().collect();
                let mut v = UnitEdgeVisitor {
                    edges: Vec::new(),
                    fn_params,
                    local_vars: HashMap::new(),
                    enclosing_owner: self.item_owner.clone(),
                };
                v.visit_block(&node.block);
                self.edges.extend(v.edges);
                return;
            }
            visit::visit_impl_item_fn(self, node);
        }

        fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
            let span = node.span();
            if span.start().line == self.target_start && span.end().line == self.target_end {
                for field in &node.fields {
                    let mut types = Vec::new();
                    collect_type_names(&field.ty, &mut types);
                    for t in types {
                        self.edges.push(EdgeKind::SignatureType { name: t });
                    }
                }
                return;
            }
            visit::visit_item_struct(self, node);
        }

        fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
            let span = node.span();
            if span.start().line == self.target_start && span.end().line == self.target_end {
                for variant in &node.variants {
                    for field in &variant.fields {
                        let mut types = Vec::new();
                        collect_type_names(&field.ty, &mut types);
                        for t in types {
                            self.edges.push(EdgeKind::SignatureType { name: t });
                        }
                    }
                }
                return;
            }
            visit::visit_item_enum(self, node);
        }

        fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
            let span = node.span();
            if span.start().line == self.target_start && span.end().line == self.target_end {
                for impl_item in &node.items {
                    if let syn::ImplItem::Fn(f) = impl_item {
                        let params = extract_fn_params(&f.sig);
                        let fn_params = params.into_iter().collect();
                        let mut v = UnitEdgeVisitor {
                            edges: Vec::new(),
                            fn_params,
                            local_vars: HashMap::new(),
                            enclosing_owner: self.item_owner.clone(),
                        };
                        v.visit_block(&f.block);
                        self.edges.extend(v.edges);
                    }
                }
                return;
            }
            visit::visit_item_impl(self, node);
        }
    }

    let mut extractor = ExtractVisitor {
        target_start: key.start,
        target_end: key.end,
        edges: &mut edges,
        item_owner,
        index,
    };
    extractor.visit_file(&file);

    (item_name, edges)
}

#[derive(Debug, Clone)]
pub enum Resolution {
    Resolved {
        target: IndexedDef,
        reason: String,
        relationship: String,
        confidence: usize,
        is_type: bool,
    },
    Ambiguous {
        name: String,
        line: usize,
        locations: Vec<String>,
    },
    UnresolvedCallback {
        name: String,
        line: usize,
    },
    UnresolvedUnknownReceiver {
        name: String,
        line: usize,
    },
    External,
}

pub fn resolve_edge(
    edge: &EdgeKind,
    from_unit: &UnitKey,
    caller_name: &str,
    index: &Index,
) -> Resolution {
    match edge {
        EdgeKind::PathCall {
            qualifier: Some(q),
            name,
            line,
        } if q == "__callback__" => Resolution::UnresolvedCallback {
            name: name.clone(),
            line: *line,
        },
        EdgeKind::PathCall {
            qualifier: Some(q),
            name,
            line,
        } => {
            if q.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                if !index.types.contains_key(q) {
                    Resolution::External
                } else if let Some(defs) = index.methods.get(&(q.clone(), name.clone())) {
                    if defs.len() == 1 {
                        Resolution::Resolved {
                            target: defs[0].clone(),
                            reason: format!("associated function on {q}"),
                            relationship: format!(
                                "associated fn of {q}, called at :{line} in {caller_name}"
                            ),
                            confidence: 1,
                            is_type: false,
                        }
                    } else {
                        Resolution::Ambiguous {
                            name: format!("{q}::{name}"),
                            line: *line,
                            locations: defs
                                .iter()
                                .map(|d| format!("{}:{}-{}", d.path, d.start, d.end))
                                .collect(),
                        }
                    }
                } else {
                    Resolution::External
                }
            } else {
                let allowed_qualifiers = ["crate", "self", "super"];
                let is_project_module = allowed_qualifiers.contains(&q.as_str())
                    || index
                        .file_sources
                        .keys()
                        .any(|p| Path::new(p).file_stem().and_then(|s| s.to_str()) == Some(q));

                if !is_project_module {
                    Resolution::External
                } else if let Some(defs) = index.free_fns.get(name) {
                    if defs.len() == 1 {
                        Resolution::Resolved {
                            target: defs[0].clone(),
                            reason: format!("resolved in module {q}"),
                            relationship: format!("callee of {caller_name} (call at :{line})"),
                            confidence: 1,
                            is_type: false,
                        }
                    } else {
                        Resolution::Ambiguous {
                            name: format!("{q}::{name}"),
                            line: *line,
                            locations: defs
                                .iter()
                                .map(|d| format!("{}:{}-{}", d.path, d.start, d.end))
                                .collect(),
                        }
                    }
                } else {
                    Resolution::External
                }
            }
        }
        EdgeKind::PathCall {
            qualifier: None,
            name,
            line,
        } => {
            if let Some(defs) = index.free_fns.get(name) {
                let same_file: Vec<_> = defs.iter().filter(|d| d.path == from_unit.path).collect();
                if same_file.len() == 1 {
                    Resolution::Resolved {
                        target: same_file[0].clone(),
                        reason: "resolved by name in same file".to_string(),
                        relationship: format!("callee of {caller_name} (call at :{line})"),
                        confidence: 1,
                        is_type: false,
                    }
                } else if let Some(matched) =
                    index.file_uses.get(&from_unit.path).and_then(|uses| {
                        uses.get(name).and_then(|imported_path| {
                            let matched: Vec<_> = defs
                                .iter()
                                .filter(|d| {
                                    let stem = Path::new(&d.path)
                                        .file_stem()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("");
                                    imported_path.split("::").any(|segment| segment == stem)
                                })
                                .collect();
                            if matched.len() == 1 {
                                Some(matched[0].clone())
                            } else {
                                None
                            }
                        })
                    })
                {
                    Resolution::Resolved {
                        target: matched,
                        reason: "resolved by use import".to_string(),
                        relationship: format!("callee of {caller_name} (call at :{line})"),
                        confidence: 1,
                        is_type: false,
                    }
                } else if defs.len() == 1 {
                    Resolution::Resolved {
                        target: defs[0].clone(),
                        reason: "resolved by unique name repo-wide".to_string(),
                        relationship: format!("callee of {caller_name} (call at :{line})"),
                        confidence: 1,
                        is_type: false,
                    }
                } else {
                    Resolution::Ambiguous {
                        name: name.clone(),
                        line: *line,
                        locations: defs
                            .iter()
                            .map(|d| format!("{}:{}-{}", d.path, d.start, d.end))
                            .collect(),
                    }
                }
            } else {
                Resolution::External
            }
        }
        EdgeKind::SelfCall { owner, name, line } => {
            if let Some(owner) = owner {
                if let Some(defs) = index.methods.get(&(owner.clone(), name.clone())) {
                    if defs.len() == 1 {
                        Resolution::Resolved {
                            target: defs[0].clone(),
                            reason: format!("resolved Self:: call on {owner}"),
                            relationship: format!(
                                "method on {owner}, called at :{line} in {caller_name}"
                            ),
                            confidence: 1,
                            is_type: false,
                        }
                    } else {
                        Resolution::Ambiguous {
                            name: format!("{owner}::{name}"),
                            line: *line,
                            locations: defs
                                .iter()
                                .map(|d| format!("{}:{}-{}", d.path, d.start, d.end))
                                .collect(),
                        }
                    }
                } else {
                    Resolution::External
                }
            } else {
                Resolution::External
            }
        }
        EdgeKind::MethodCallTyped {
            receiver_type,
            method,
            line,
            source_var,
        } => {
            if let Some(defs) = index.methods.get(&(receiver_type.clone(), method.clone())) {
                if defs.len() == 1 {
                    Resolution::Resolved {
                        target: defs[0].clone(),
                        reason: format!("receiver type from {source_var}"),
                        relationship: format!(
                            "method on {receiver_type}, called at :{line} in {caller_name}"
                        ),
                        confidence: 1,
                        is_type: false,
                    }
                } else {
                    Resolution::Ambiguous {
                        name: format!("{receiver_type}.{method}"),
                        line: *line,
                        locations: defs
                            .iter()
                            .map(|d| format!("{}:{}-{}", d.path, d.start, d.end))
                            .collect(),
                    }
                }
            } else {
                Resolution::External
            }
        }
        EdgeKind::MethodCallUnknown { method, line } => Resolution::UnresolvedUnknownReceiver {
            name: method.clone(),
            line: *line,
        },
        EdgeKind::MacroCall { name, line } => {
            if let Some(defs) = index.free_fns.get(name) {
                if defs.len() == 1 {
                    Resolution::Resolved {
                        target: defs[0].clone(),
                        reason: "macro-argument candidate".to_string(),
                        relationship: format!(
                            "callee of {caller_name} inside macro (call at :{line})"
                        ),
                        confidence: 3,
                        is_type: false,
                    }
                } else {
                    Resolution::Ambiguous {
                        name: name.clone(),
                        line: *line,
                        locations: defs
                            .iter()
                            .map(|d| format!("{}:{}-{}", d.path, d.start, d.end))
                            .collect(),
                    }
                }
            } else {
                Resolution::External
            }
        }
        EdgeKind::FnAsValue { name, line } => {
            if let Some(defs) = index.free_fns.get(name) {
                if defs.len() == 1 {
                    Resolution::Resolved {
                        target: defs[0].clone(),
                        reason: "function used as a value".to_string(),
                        relationship: format!("function referenced in {caller_name} at :{line}"),
                        confidence: 2,
                        is_type: false,
                    }
                } else {
                    Resolution::Ambiguous {
                        name: name.clone(),
                        line: *line,
                        locations: defs
                            .iter()
                            .map(|d| format!("{}:{}-{}", d.path, d.start, d.end))
                            .collect(),
                    }
                }
            } else {
                Resolution::External
            }
        }
        EdgeKind::SignatureType { name } => {
            if let Some(defs) = index.types.get(name) {
                if defs.len() == 1 {
                    Resolution::Resolved {
                        target: defs[0].clone(),
                        reason: format!("type in signature of {caller_name}"),
                        relationship: format!("type in signature of {caller_name}"),
                        confidence: 1,
                        is_type: true,
                    }
                } else {
                    Resolution::Ambiguous {
                        name: name.clone(),
                        line: 0,
                        locations: defs
                            .iter()
                            .map(|d| format!("{}:{}-{}", d.path, d.start, d.end))
                            .collect(),
                    }
                }
            } else {
                Resolution::External
            }
        }
    }
}

fn locations(defs: &[&IndexedDef]) -> Vec<String> {
    defs.iter()
        .map(|d| format!("{}:{}-{}", d.path, d.start, d.end))
        .collect()
}

/// Resolves one call of a non-Rust unit by name. There are no types and no
/// imports: a plain call is a function of the same file when that is unique,
/// else the one function of that name in the repository. `self.f()` and
/// `this.f()` are looked up in the same file only. A call through any other
/// receiver is listed, not followed.
fn resolve_generic_call(
    call: &CallEdge,
    language: usize,
    from: &UnitKey,
    caller_name: &str,
    index: &Index,
) -> Resolution {
    let Some(defs) = index.generic.fns.get(&(language, call.name.clone())) else {
        return Resolution::External;
    };
    let relationship = format!("callee of {caller_name} (call at :{})", call.line);
    let found = |target: &IndexedDef, reason: &str| Resolution::Resolved {
        target: target.clone(),
        reason: reason.to_string(),
        relationship: relationship.clone(),
        confidence: 1,
        is_type: false,
    };
    let ambiguous = |defs: &[&IndexedDef]| Resolution::Ambiguous {
        name: call.name.clone(),
        line: call.line,
        locations: locations(defs),
    };
    let same_file: Vec<&IndexedDef> = defs.iter().filter(|d| d.path == from.path).collect();

    if call.member {
        if !is_own_receiver(call.receiver.as_deref()) {
            return Resolution::UnresolvedUnknownReceiver {
                name: call.name.clone(),
                line: call.line,
            };
        }
        return match same_file.as_slice() {
            [one] => found(one, "resolved by name on self/this in the same file"),
            [] => Resolution::UnresolvedUnknownReceiver {
                name: call.name.clone(),
                line: call.line,
            },
            many => ambiguous(many),
        };
    }

    match same_file.as_slice() {
        [one] => found(one, "resolved by name in same file"),
        [] => match defs.as_slice() {
            [one] => found(one, "resolved by unique name repo-wide"),
            many => ambiguous(&many.iter().collect::<Vec<_>>()),
        },
        many => ambiguous(many),
    }
}

/// The calls of a non-Rust unit, resolved; each with the name to count when it
/// turns out to be external.
fn generic_resolutions(unit: &UnitKey, index: &Index) -> Vec<(Resolution, Option<String>)> {
    let Some((language, calls)) = index.generic.calls.get(&unit.path) else {
        return Vec::new();
    };
    let name = index
        .generic
        .defs
        .get(&unit.path)
        .and_then(|defs| defs.iter().find(|d| d.start == unit.start && d.end == unit.end))
        .map_or_else(|| "seed".to_string(), |d| d.bare_name().to_string());
    calls
        .iter()
        .filter(|call| unit.start <= call.line && call.line <= unit.end)
        .map(|call| {
            (
                resolve_generic_call(call, *language, unit, &name, index),
                Some(call.name.clone()),
            )
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub target: IndexedDef,
    pub relationship: String,
    pub reason: String,
    pub confidence: usize,
    pub is_type: bool,
    pub referrers: usize,
    pub from_path: String,
    pub is_leaf: bool,
}

pub fn rank_key(
    cand: &Candidate,
    index: &Index,
) -> (
    usize,
    usize,
    usize,
    isize,
    usize,
    usize,
    String,
    usize,
    usize,
    String,
) {
    let conf = cand.confidence;
    let edge_kind_rank = if cand.is_type { 2 } else { 1 };

    let locality = if cand.target.path == cand.from_path {
        1
    } else if Path::new(&cand.target.path).parent() == Path::new(&cand.from_path).parent() {
        2
    } else {
        3
    };

    let referrers_rank = -(cand.referrers as isize);

    let callers_count = index
        .total_callers_count
        .get(&cand.target.unit_key())
        .copied()
        .unwrap_or(0);
    let utility_rank = if callers_count >= UTILITY_THRESHOLD {
        2
    } else {
        1
    };

    let size = cand.target.end.saturating_sub(cand.target.start);

    (
        utility_rank,
        conf,
        edge_kind_rank,
        referrers_rank,
        locality,
        size,
        cand.target.path.clone(),
        cand.target.start,
        cand.target.end,
        cand.target.name.clone(),
    )
}

#[derive(Debug, Clone)]
pub struct OmittedItem {
    pub key: UnitKey,
    pub name: String,
    pub relationship: String,
    pub lines: usize,
    pub reason: String,
}

pub fn render_unit_lines(source: &str, start: usize, end: usize) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = String::new();
    for n in start..=end {
        if n <= lines.len() {
            out.push_str(&format!("  {n:>5} | {}\n", lines[n - 1]));
        }
    }
    out
}

#[cfg(test)]
pub fn run(
    matching_seeds: &[MatchingSeed],
    dir: Option<&Path>,
    max_depth: usize,
) -> String {
    run_with_reserved_chars(matching_seeds, dir, max_depth, 0)
}

pub fn run_with_reserved_chars(
    matching_seeds: &[MatchingSeed],
    dir: Option<&Path>,
    max_depth: usize,
    reserved_chars: usize,
) -> String {
    let output_limit = MAX_ADDED_CHARS.saturating_sub(reserved_chars);
    let index = build_index(dir);

    let mut seeds = Vec::new();
    let mut non_rust_or_test_seeds = 0;

    for (u, _) in matching_seeds {
        if !u.key.path.ends_with(".rs") {
            // Another language is followed when it is indexed (so not a test
            // file) and the hit is a unit.
            if u.label != "Line" && index.generic.calls.contains_key(&u.key.path) {
                seeds.push(u.key.clone());
            } else {
                non_rust_or_test_seeds += 1;
            }
            continue;
        }

        let in_test = if let Some(src) = index.file_sources.get(&u.key.path) {
            if let Ok(file) = syn::parse_file(src) {
                pull::rust_items(&file)
                    .iter()
                    .any(|it| it.start == u.key.start && it.end == u.key.end && it.in_test_mod)
            } else {
                false
            }
        } else {
            false
        };

        if in_test || u.label == "Line" {
            non_rust_or_test_seeds += 1;
        } else {
            seeds.push(u.key.clone());
        }
    }

    let mut included: HashSet<UnitKey> =
        matching_seeds.iter().map(|(u, _)| u.key.clone()).collect();
    let mut frontier: Vec<UnitKey> = seeds.clone();

    let mut admitted_by_depth: BTreeMap<usize, Vec<(Candidate, String)>> = BTreeMap::new();
    let mut omitted_items: Vec<OmittedItem> = Vec::new();
    let mut unresolved_callbacks: Vec<(String, String, usize)> = Vec::new();
    let mut unresolved_unknown_receivers: Vec<(String, String, usize)> = Vec::new();
    let mut ambiguous_items: Vec<(String, String, usize, Vec<String>)> = Vec::new();
    let mut seen_unresolved: HashSet<String> = HashSet::new();
    let mut external_count: HashSet<String> = HashSet::new();

    let mut total_added_units = 0;
    let mut total_added_chars = 0;
    let mut primary_stop_reason: Option<String> = None;

    for depth in 1..=max_depth {
        if frontier.is_empty() {
            break;
        }

        let mut level_cands: HashMap<UnitKey, Candidate> = HashMap::new();

        frontier.sort();
        for u in &frontier {
            let Some(source) = index.source(&u.path) else {
                continue;
            };

            let resolutions: Vec<(Resolution, Option<String>)> = if u.path.ends_with(".rs") {
                let (caller_name, edges) = extract_unit_edges(source, u, &index);
                let caller_display = caller_name.unwrap_or_else(|| "seed".to_string());
                edges
                    .into_iter()
                    .map(|edge| {
                        let resolution = resolve_edge(&edge, u, &caller_display, &index);
                        let name = match edge {
                            EdgeKind::PathCall { name, .. }
                            | EdgeKind::MethodCallTyped { method: name, .. }
                            | EdgeKind::MethodCallUnknown { method: name, .. }
                            | EdgeKind::MacroCall { name, .. }
                            | EdgeKind::FnAsValue { name, .. } => Some(name),
                            _ => None,
                        };
                        (resolution, name)
                    })
                    .collect()
            } else {
                generic_resolutions(u, &index)
            };

            for (resolution, external_name) in resolutions {
                match resolution {
                    Resolution::Resolved {
                        target,
                        reason,
                        relationship,
                        confidence,
                        is_type,
                    } => {
                        let t_key = target.unit_key();
                        if included.contains(&t_key) {
                            continue;
                        }

                        let callers_count =
                            index.total_callers_count.get(&t_key).copied().unwrap_or(0);
                        let is_leaf = callers_count >= UTILITY_THRESHOLD;
                        let cand_reason = if is_leaf {
                            format!("utility, called from {callers_count} units")
                        } else {
                            reason
                        };

                        level_cands
                            .entry(t_key)
                            .and_modify(|c| {
                                c.referrers += 1;
                                if confidence < c.confidence {
                                    c.confidence = confidence;
                                    c.reason = cand_reason.clone();
                                    c.relationship = relationship.clone();
                                }
                            })
                            .or_insert(Candidate {
                                target,
                                relationship,
                                reason: cand_reason,
                                confidence,
                                is_type,
                                referrers: 1,
                                from_path: u.path.clone(),
                                is_leaf,
                            });
                    }
                    Resolution::Ambiguous {
                        name,
                        line,
                        locations,
                    } => {
                        if seen_unresolved.insert(name.clone()) {
                            ambiguous_items.push((name, u.path.clone(), line, locations));
                        }
                    }
                    Resolution::UnresolvedCallback { name, line } => {
                        if seen_unresolved.insert(name.clone()) {
                            unresolved_callbacks.push((name, u.path.clone(), line));
                        }
                    }
                    Resolution::UnresolvedUnknownReceiver { name, line } => {
                        if seen_unresolved.insert(name.clone()) {
                            unresolved_unknown_receivers.push((name, u.path.clone(), line));
                        }
                    }
                    Resolution::External => {
                        if let Some(n) = external_name {
                            external_count.insert(n);
                        }
                    }
                }
            }
        }

        let mut sorted_cands: Vec<Candidate> = level_cands.into_values().collect();
        sorted_cands.sort_by_key(|a| rank_key(a, &index));

        let mut next_frontier = Vec::new();
        let mut admitted_this_depth = Vec::new();

        for cand in sorted_cands {
            let line_count = cand.target.end.saturating_sub(cand.target.start) + 1;
            if line_count > MAX_UNIT_LINES {
                omitted_items.push(OmittedItem {
                    key: cand.target.unit_key(),
                    name: cand.target.name.clone(),
                    relationship: cand.relationship.clone(),
                    lines: line_count,
                    reason: "unit-too-large".to_string(),
                });
                continue;
            }

            if total_added_units >= MAX_ADDED_UNITS {
                if primary_stop_reason.is_none() {
                    primary_stop_reason =
                        Some(format!("unit budget ({MAX_ADDED_UNITS}) at depth {depth}"));
                }
                omitted_items.push(OmittedItem {
                    key: cand.target.unit_key(),
                    name: cand.target.name.clone(),
                    relationship: cand.relationship.clone(),
                    lines: line_count,
                    reason: "budget:units".to_string(),
                });
                continue;
            }

            let Some(src) = index.source(&cand.target.path) else {
                continue;
            };
            let body = render_unit_lines(src, cand.target.start, cand.target.end);
            let unit_header = format!(
                "  {}: {}:{}-{}\n  cascade: depth {depth} | {} | {}\n",
                cand.target.label,
                cand.target.path,
                cand.target.start,
                cand.target.end,
                cand.relationship,
                cand.reason
            );
            let rendered_len = unit_header.chars().count() + body.chars().count();

            if total_added_chars + rendered_len
                > (output_limit.saturating_sub(REPORT_RESERVE_CHARS))
            {
                if primary_stop_reason.is_none() {
                    primary_stop_reason = Some(format!(
                        "character budget ({output_limit}) at depth {depth}"
                    ));
                }
                omitted_items.push(OmittedItem {
                    key: cand.target.unit_key(),
                    name: cand.target.name.clone(),
                    relationship: cand.relationship.clone(),
                    lines: line_count,
                    reason: "budget:chars".to_string(),
                });
                continue;
            }

            total_added_units += 1;
            total_added_chars += rendered_len;
            included.insert(cand.target.unit_key());

            if !cand.is_leaf && !cand.is_type {
                next_frontier.push(cand.target.unit_key());
            }

            admitted_this_depth.push((cand, body));
        }

        if !admitted_this_depth.is_empty() {
            admitted_by_depth.insert(depth, admitted_this_depth);
        }

        frontier = next_frontier;
    }

    let mut out = String::new();

    for (d, items) in admitted_by_depth {
        let count = items.len();
        let unit_str = if count == 1 { "unit" } else { "units" };
        out.push_str(&format!("\n== CASCADE depth {d} ({count} {unit_str}) ==\n"));
        for (cand, body) in items {
            out.push_str(&format!(
                "  {}: {}:{}-{}\n  cascade: depth {d} | {} | {}\n",
                cand.target.label,
                cand.target.path,
                cand.target.start,
                cand.target.end,
                cand.relationship,
                cand.reason
            ));
            out.push_str(&body);
        }
    }

    let stop_reason = if let Some(r) = primary_stop_reason {
        r
    } else if frontier.is_empty() {
        "converged".to_string()
    } else {
        format!("depth limit ({max_depth})")
    };

    out.push_str("\n== CASCADE report ==\n");
    let omitted_str = if omitted_items.is_empty() {
        "Nothing omitted.".to_string()
    } else {
        format!("Omitted ({}):", omitted_items.len())
    };

    out.push_str(&format!(
        "Stopped: {stop_reason}. Budget: {total_added_units} of {MAX_ADDED_UNITS} added units, {total_added_chars} of {output_limit} chars. {omitted_str}\n"
    ));

    let first_seed_name = seeds
        .first()
        .and_then(|k| {
            if let Some(defs) = index.generic.defs.get(&k.path) {
                return defs
                    .iter()
                    .find(|d| d.start == k.start && d.end == k.end)
                    .map(|d| d.bare_name().to_string());
            }
            let src = index.file_sources.get(&k.path)?;
            let file = syn::parse_file(src).ok()?;
            pull::rust_items(&file)
                .into_iter()
                .find(|it| it.start == k.start && it.end == k.end)
                .and_then(|it| it.name)
        })
        .unwrap_or_else(|| "IDENT".to_string());

    let footer_disclaimer = "Analysis is syntactic (no types, trait dispatch, generics, macro expansion or cfg). A dependency missing from this bundle may still exist.\n";
    let scope_note = format!(
        "Not followed, by scope: callers, tests, trait impls. See `cvb pull callers:{first_seed_name}`.\n"
    );
    let mandatory_footer_reserve =
        footer_disclaimer.chars().count() + scope_note.chars().count() + 100;

    let mut omitted_shown = 0;
    for item in &omitted_items {
        let line = format!(
            "  {}:{}-{}  {:16}  {:24}  {:3} lines  {:12}  → cvb pull file:{} lines:{}-{}\n",
            item.key.path,
            item.key.start,
            item.key.end,
            item.name,
            item.relationship,
            item.lines,
            item.reason,
            item.key.path,
            item.key.start,
            item.key.end
        );
        if out.chars().count() + line.chars().count() + mandatory_footer_reserve + 80
            <= output_limit
        {
            out.push_str(&line);
            omitted_shown += 1;
        } else {
            let hidden = omitted_items.len() - omitted_shown;
            out.push_str(&format!(
                "  ... and {hidden} more omitted unit(s) not listed (reserve limit reached)\n"
            ));
            break;
        }
    }

    let total_unresolved =
        unresolved_callbacks.len() + unresolved_unknown_receivers.len() + ambiguous_items.len();
    if total_unresolved > 0 {
        let header = format!("Not followed, unresolved ({total_unresolved}):\n");
        if out.chars().count() + header.chars().count() + mandatory_footer_reserve <= output_limit
        {
            out.push_str(&header);
            let mut unres_shown = 0;
            let mut hit_reserve = false;

            for (name, path, line) in &unresolved_callbacks {
                let entry = format!(
                    "  {name:16}  {path}:{line:<5}  call through parameter `{name}` (callback; target not statically known)\n"
                );
                if out.chars().count() + entry.chars().count() + mandatory_footer_reserve + 80
                    <= output_limit
                {
                    out.push_str(&entry);
                    unres_shown += 1;
                } else {
                    hit_reserve = true;
                    break;
                }
            }
            if !hit_reserve {
                for (name, path, line) in &unresolved_unknown_receivers {
                    let entry = format!(
                        "  {name:16}  {path}:{line:<5}  method call, receiver type unknown\n"
                    );
                    if out.chars().count() + entry.chars().count() + mandatory_footer_reserve + 80
                        <= output_limit
                    {
                        out.push_str(&entry);
                        unres_shown += 1;
                    } else {
                        hit_reserve = true;
                        break;
                    }
                }
            }
            if !hit_reserve {
                for (name, path, line, locs) in &ambiguous_items {
                    let loc_str = if locs.len() > 3 {
                        format!(
                            ": {}, ... and {} more",
                            locs[..3].join(", "),
                            locs.len() - 3
                        )
                    } else if !locs.is_empty() {
                        format!(": {}", locs.join(", "))
                    } else {
                        String::new()
                    };
                    let entry = format!(
                        "  {name:16}  {path}:{line:<5}  ambiguous ({} candidates{loc_str})\n",
                        locs.len()
                    );
                    if out.chars().count() + entry.chars().count() + mandatory_footer_reserve + 80
                        <= output_limit
                    {
                        out.push_str(&entry);
                        unres_shown += 1;
                    } else {
                        hit_reserve = true;
                        break;
                    }
                }
            }
            if hit_reserve {
                let hidden = total_unresolved - unres_shown;
                out.push_str(&format!("  ... and {hidden} more unresolved item(s) not listed (reserve limit reached)\n"));
            }
        }
    }

    if !external_count.is_empty() {
        out.push_str(&format!(
            "Not followed, external or std: {} distinct names, not listed.\n",
            external_count.len()
        ));
    }

    if non_rust_or_test_seeds > 0 {
        out.push_str(&format!(
            "{non_rust_or_test_seeds} initial units not followed (test code, no function or type unit, or a file type cascade does not read).\n"
        ));
    }

    out.push_str(&scope_note);

    for warning in &index.parse_warnings {
        if out.chars().count() + warning.chars().count() + footer_disclaimer.chars().count() + 25
            <= output_limit
        {
            out.push_str(&format!("Index incomplete: {warning}\n"));
        }
    }

    out.push_str(footer_disclaimer);

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pull::SearchUnit;

    #[test]
    fn total_cascade_output_strictly_bounded_by_24000_chars() {
        let dir = std::env::temp_dir().join(format!("cvb-test-hardcap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let run_cmd = |args: &[&str]| {
            std::process::Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .output()
                .unwrap();
        };

        run_cmd(&["git", "init"]);
        run_cmd(&["git", "config", "user.name", "CVB Test"]);
        run_cmd(&["git", "config", "user.email", "test@cvb.local"]);

        // Create 40 distinct functions to severely stress unit limits and generate 30+ omitted items
        let mut source = String::from("pub fn root() {\n");
        for i in 0..40 {
            source.push_str(&format!("    fn_{i}();\n"));
        }
        source.push_str("}\n\n");
        for i in 0..40 {
            source.push_str(&format!("pub fn fn_{i}() {{\n    let _x = {i};\n}}\n"));
        }

        std::fs::write(dir.join("src/lib.rs"), &source).unwrap();
        run_cmd(&["git", "add", "src/lib.rs"]);
        run_cmd(&["git", "commit", "-m", "init"]);

        let matching_seeds = vec![(
            SearchUnit {
                key: UnitKey {
                    path: "src/lib.rs".to_string(),
                    start: 1,
                    end: 42,
                },
                label: "Rust function",
                warning: None,
            },
            vec![("root".to_string(), vec![1])],
        )];

        let output = run(&matching_seeds, Some(&dir), 1);
        let reserved_chars = 137;
        let reserved_output =
            run_with_reserved_chars(&matching_seeds, Some(&dir), 1, reserved_chars);
        let _ = std::fs::remove_dir_all(&dir);

        let total_chars = output.chars().count();
        assert!(
            total_chars <= MAX_ADDED_CHARS,
            "Total cascade addition output must strictly not exceed {MAX_ADDED_CHARS} chars, got {total_chars}"
        );
        assert!(output.contains("== CASCADE report =="));
        assert!(
            output.contains("Analysis is syntactic"),
            "Footer disclaimer must be preserved"
        );

        let reserved_total_chars = reserved_output.chars().count();
        assert!(
            reserved_total_chars <= MAX_ADDED_CHARS - reserved_chars,
            "Reserved cascade output must leave {reserved_chars} chars available, got {reserved_total_chars}"
        );
        assert!(reserved_output.contains("== CASCADE report =="));
        assert!(
            reserved_output.contains("Analysis is syntactic"),
            "Reserved output must preserve the footer disclaimer"
        );
    }

    #[test]
    fn nested_generic_signature_types_are_extracted() {
        let dir = std::env::temp_dir().join(format!("cvb-test-generic-sig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let run_cmd = |args: &[&str]| {
            std::process::Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .output()
                .unwrap();
        };

        run_cmd(&["git", "init"]);
        run_cmd(&["git", "config", "user.name", "CVB Test"]);
        run_cmd(&["git", "config", "user.email", "test@cvb.local"]);

        let source = r#"
pub struct TargetStruct {
    pub id: usize,
}

pub fn producer() -> Result<Vec<TargetStruct>, String> {
    Ok(Vec::new())
}
"#;
        std::fs::write(dir.join("src/lib.rs"), source).unwrap();
        run_cmd(&["git", "add", "src/lib.rs"]);
        run_cmd(&["git", "commit", "-m", "init"]);

        let index = build_index(Some(&dir));
        let producer_key = UnitKey {
            path: "src/lib.rs".to_string(),
            start: 6,
            end: 8,
        };
        let (_, edges) = extract_unit_edges(source, &producer_key, &index);

        let has_target_sig = edges
            .iter()
            .any(|e| matches!(e, EdgeKind::SignatureType { name } if name == "TargetStruct"));
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            has_target_sig,
            "Expected TargetStruct inside Result<Vec<TargetStruct>, String> to be extracted as a signature type"
        );
    }

    #[test]
    fn unresolved_dependencies_are_deduplicated_by_name() {
        let dir = std::env::temp_dir().join(format!("cvb-test-unres-dedup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let run_cmd = |args: &[&str]| {
            std::process::Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .output()
                .unwrap();
        };

        run_cmd(&["git", "init"]);
        run_cmd(&["git", "config", "user.name", "CVB Test"]);
        run_cmd(&["git", "config", "user.email", "test@cvb.local"]);

        let source = r#"
pub fn multi_callback<F>(cb: F) where F: Fn() {
    cb();
    cb();
    cb();
}
"#;
        std::fs::write(dir.join("src/lib.rs"), source).unwrap();
        run_cmd(&["git", "add", "src/lib.rs"]);
        run_cmd(&["git", "commit", "-m", "init"]);

        let matching_seeds = vec![(
            SearchUnit {
                key: UnitKey {
                    path: "src/lib.rs".to_string(),
                    start: 2,
                    end: 6,
                },
                label: "Rust function",
                warning: None,
            },
            vec![("multi_callback".to_string(), vec![2])],
        )];

        let output = run(&matching_seeds, Some(&dir), 1);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            output.contains("Not followed, unresolved (1):"),
            "Multiple calls to `cb` should be deduplicated to 1 distinct unresolved dependency:\n{output}"
        );
        assert_eq!(
            output.matches("call through parameter `cb`").count(),
            1,
            "Only one entry for `cb` should appear in report:\n{output}"
        );
    }

    #[test]
    fn import_resolution_uses_exact_path_components_not_substrings() {
        let dir =
            std::env::temp_dir().join(format!("cvb-test-import-exact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let run_cmd = |args: &[&str]| {
            std::process::Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .output()
                .unwrap();
        };

        run_cmd(&["git", "init"]);
        run_cmd(&["git", "config", "user.name", "CVB Test"]);
        run_cmd(&["git", "config", "user.email", "test@cvb.local"]);

        // bar.rs has target(), foobar.rs also has target()
        std::fs::write(
            dir.join("src/bar.rs"),
            "pub fn target() -> usize { 1 }
",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/foobar.rs"),
            "pub fn target() -> usize { 2 }
",
        )
        .unwrap();
        let main_src = "use crate::bar::target;
fn caller() {
    target();
}
";
        std::fs::write(dir.join("src/main.rs"), main_src).unwrap();

        run_cmd(&["git", "add", "src/bar.rs", "src/foobar.rs", "src/main.rs"]);
        run_cmd(&["git", "commit", "-m", "init"]);

        let index = build_index(Some(&dir));
        let caller_key = UnitKey {
            path: "src/main.rs".to_string(),
            start: 2,
            end: 4,
        };
        let (_, edges) = extract_unit_edges(main_src, &caller_key, &index);
        let res = resolve_edge(&edges[0], &caller_key, "caller", &index);

        let _ = std::fs::remove_dir_all(&dir);

        match res {
            Resolution::Resolved { target, reason, .. } => {
                assert_eq!(target.path, "src/bar.rs");
                assert!(reason.contains("resolved by use import"));
            }
            other => panic!("Expected exact import to resolve to src/bar.rs, got {other:?}"),
        }
    }

    #[test]
    fn compound_cfg_test_attributes_are_excluded_from_index() {
        let dir =
            std::env::temp_dir().join(format!("cvb-test-cfg-compound-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let run_cmd = |args: &[&str]| {
            std::process::Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .output()
                .unwrap();
        };

        run_cmd(&["git", "init"]);
        run_cmd(&["git", "config", "user.name", "CVB Test"]);
        run_cmd(&["git", "config", "user.email", "test@cvb.local"]);

        let source = r#"
#[cfg(all(test, feature = "dummy"))]
fn compound_test_fn() {}

#[cfg(any(test, windows))]
fn any_test_fn() {}

#[cfg(not(test))]
fn production_fn() {}
"#;
        std::fs::write(dir.join("src/lib.rs"), source).unwrap();
        run_cmd(&["git", "add", "src/lib.rs"]);
        run_cmd(&["git", "commit", "-m", "init"]);

        let index = build_index(Some(&dir));
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            !index.free_fns.contains_key("compound_test_fn"),
            "compound_test_fn should be excluded"
        );
        assert!(
            !index.free_fns.contains_key("any_test_fn"),
            "any_test_fn should be excluded"
        );
        assert!(
            index.free_fns.contains_key("production_fn"),
            "production_fn should be included"
        );
    }

    #[test]
    fn self_call_resolves_to_correct_enclosing_owner() {
        let dir = std::env::temp_dir().join(format!("cvb-test-selfcall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let run_cmd = |args: &[&str]| {
            std::process::Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .output()
                .unwrap();
        };

        run_cmd(&["git", "init"]);
        run_cmd(&["git", "config", "user.name", "CVB Test"]);
        run_cmd(&["git", "config", "user.email", "test@cvb.local"]);

        let source = r#"
struct Alpha;
impl Alpha {
    fn helper() -> usize { 1 }
    fn run() -> usize {
        Self::helper()
    }
}

struct Beta;
impl Beta {
    fn helper() -> usize { 2 }
    fn run() -> usize {
        Self::helper()
    }
}
"#;
        std::fs::write(dir.join("src/lib.rs"), source).unwrap();
        run_cmd(&["git", "add", "src/lib.rs"]);
        run_cmd(&["git", "commit", "-m", "init"]);

        let index = build_index(Some(&dir));

        let alpha_run = &index
            .methods
            .get(&("Alpha".to_string(), "run".to_string()))
            .unwrap()[0];
        let (_, alpha_edges) = extract_unit_edges(source, &alpha_run.unit_key(), &index);
        let alpha_res = resolve_edge(&alpha_edges[0], &alpha_run.unit_key(), "run", &index);

        let beta_run = &index
            .methods
            .get(&("Beta".to_string(), "run".to_string()))
            .unwrap()[0];
        let (_, beta_edges) = extract_unit_edges(source, &beta_run.unit_key(), &index);
        let beta_res = resolve_edge(&beta_edges[0], &beta_run.unit_key(), "run", &index);

        let _ = std::fs::remove_dir_all(&dir);

        match alpha_res {
            Resolution::Resolved { target, .. } => {
                assert_eq!(target.owner.as_deref(), Some("Alpha"));
            }
            other => panic!("Expected Alpha::run to resolve to Alpha::helper, got {other:?}"),
        }

        match beta_res {
            Resolution::Resolved { target, .. } => {
                assert_eq!(target.owner.as_deref(), Some("Beta"));
            }
            other => panic!("Expected Beta::run to resolve to Beta::helper, got {other:?}"),
        }
    }

    #[test]
    fn caller_tally_attributes_calls_only_to_calling_unit() {
        let dir = std::env::temp_dir().join(format!("cvb-test-tally-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let run_cmd = |args: &[&str]| {
            std::process::Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .output()
                .unwrap();
        };

        run_cmd(&["git", "init"]);
        run_cmd(&["git", "config", "user.name", "CVB Test"]);
        run_cmd(&["git", "config", "user.email", "test@cvb.local"]);

        let source = r#"
fn target_helper() {}

fn caller_one() {
    target_helper();
}

fn innocent_bystander_a() {}
fn innocent_bystander_b() {}
fn innocent_bystander_c() {}
"#;
        std::fs::write(dir.join("src/lib.rs"), source).unwrap();
        run_cmd(&["git", "add", "src/lib.rs"]);
        run_cmd(&["git", "commit", "-m", "init"]);

        let index = build_index(Some(&dir));
        let target_def = &index.free_fns.get("target_helper").unwrap()[0];
        let caller_count = index
            .total_callers_count
            .get(&target_def.unit_key())
            .copied()
            .unwrap_or(0);

        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            caller_count, 1,
            "target_helper should have exactly 1 caller, got {caller_count}"
        );
    }

    #[test]
    fn parse_type_name_unwraps_references() {
        let ty: syn::Type = syn::parse_str("&ClipboardBuffer").unwrap();
        assert_eq!(
            extract_type_name_from_type(&ty).as_deref(),
            Some("ClipboardBuffer")
        );

        let mut_ty: syn::Type = syn::parse_str("&mut Example").unwrap();
        assert_eq!(
            extract_type_name_from_type(&mut_ty).as_deref(),
            Some("Example")
        );
    }

    #[test]
    fn edge_extraction_ignores_comments_and_strings() {
        let source = r#"fn target() {
            // run();
            let _text = "run()";
        }"#;
        let key = UnitKey {
            path: "test.rs".to_string(),
            start: 1,
            end: 4,
        };
        let index = Index {
            free_fns: HashMap::new(),
            methods: HashMap::new(),
            types: HashMap::new(),
            traits: HashMap::new(),
            file_uses: HashMap::new(),
            file_sources: [("test.rs".to_string(), source.to_string())].into(),
            parse_warnings: Vec::new(),
            total_callers_count: HashMap::new(),
            generic: GenericIndex::default(),
        };

        let (_, edges) = extract_unit_edges(source, &key, &index);
        assert!(edges.is_empty(), "expected no edges from comments/strings");
    }

    #[test]
    fn edge_extraction_finds_path_call_and_typed_receiver_method() {
        let source = r#"fn target(buffer: &ClipboardBuffer) {
            chunk_messages();
            buffer.len();
        }"#;
        let key = UnitKey {
            path: "test.rs".to_string(),
            start: 1,
            end: 4,
        };
        let index = Index {
            free_fns: HashMap::new(),
            methods: HashMap::new(),
            types: HashMap::new(),
            traits: HashMap::new(),
            file_uses: HashMap::new(),
            file_sources: [("test.rs".to_string(), source.to_string())].into(),
            parse_warnings: Vec::new(),
            total_callers_count: HashMap::new(),
            generic: GenericIndex::default(),
        };

        let (_, edges) = extract_unit_edges(source, &key, &index);
        assert!(
            edges
                .iter()
                .any(|e| matches!(e, EdgeKind::PathCall { name, .. } if name == "chunk_messages"))
        );
        assert!(edges.iter().any(|e| matches!(e, EdgeKind::MethodCallTyped { receiver_type, method, .. } if receiver_type == "ClipboardBuffer" && method == "len")));
    }

    /// A temporary repository with `files` committed, for `cvb pull ... cascade:N`.
    fn repo_with(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cvb-test-gc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
                .output()
                .unwrap();
        };
        git(&["init", "-q"]);
        for (path, text) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        git(&["add", "-A"]);
        dir
    }

    fn cascade_of(dir: &std::path::Path, args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let (code, out, err) = crate::pull::run_capture(&args, Some(dir));
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(code, 0, "{out}{err}");
        out
    }

    #[test]
    fn cascade_follows_calls_in_python_and_names_how_it_resolved_them() {
        let dir = repo_with(
            "py",
            &[
                ("app.py", "def main():\n    helper()\n    other.thing()\n    print(1)\n"),
                ("lib.py", "def helper():\n    deep()\n\ndef deep():\n    return 1\n"),
            ],
        );
        let out = cascade_of(&dir, &["def main", "cascade:2"]);
        assert!(out.contains("== CASCADE depth 1 (1 unit) =="), "{out}");
        assert!(out.contains("Python function: lib.py:1-2"), "{out}");
        assert!(out.contains("cascade: depth 1 | callee of main (call at :2) | resolved by unique name repo-wide"), "{out}");
        assert!(out.contains("== CASCADE depth 2 (1 unit) ==") && out.contains("Python function: lib.py:4-5"), "{out}");
        // `other.thing()` and `print()` name nothing in the repository: external, not listed.
        assert!(out.contains("Not followed, external or std: 2 distinct names"), "{out}");
        assert!(!out.contains("unresolved"), "{out}");
        assert!(out.contains("Stopped: depth limit (2)."), "{out}");
    }

    #[test]
    fn cascade_prefers_the_same_file_and_reports_ambiguity() {
        let dir = repo_with(
            "go",
            &[
                ("a.go", "package p\n\nfunc Main() {\n\tlocal()\n\tshared()\n}\n\nfunc local() {}\n\nfunc shared() {}\n"),
                ("b.go", "package p\n\nfunc shared() {}\n"),
            ],
        );
        let out = cascade_of(&dir, &["func Main", "cascade:1"]);
        assert!(out.contains("Go function: a.go:8-8") && out.contains("resolved by name in same file"), "{out}");
        // `shared` exists in both files, one of them next to the caller: same file wins.
        assert!(out.contains("Go function: a.go:10-10"), "{out}");
        assert!(!out.contains("b.go:3-3"), "{out}");
    }

    #[test]
    fn cascade_lists_an_ambiguous_name_instead_of_guessing() {
        let dir = repo_with(
            "amb",
            &[
                ("main.js", "export function start() {\n  load();\n}\n"),
                ("x.js", "export function load() {}\n"),
                ("y.js", "export function load() {}\n"),
            ],
        );
        let out = cascade_of(&dir, &["function start", "cascade:1"]);
        assert!(out.contains("Not followed, unresolved (1):"), "{out}");
        assert!(out.contains("load") && out.contains("ambiguous (2 candidates"), "{out}");
        assert!(!out.contains("== CASCADE depth 1"), "{out}");
    }

    #[test]
    fn cascade_follows_self_and_this_calls_but_lists_other_receivers() {
        let dir = repo_with(
            "self",
            &[(
                "a.py",
                "class A:\n    def run(self):\n        self.step()\n        dep.step()\n        dep.known()\n\n    def step(self):\n        pass\n\n    def known(self):\n        pass\n",
            )],
        );
        let out = cascade_of(&dir, &["def run", "cascade:1"]);
        assert!(out.contains("Python function: a.py:7-8"), "{out}");
        assert!(out.contains("resolved by name on self/this in the same file"), "{out}");
        // `dep.known()` could be `A.known`, but the receiver is not `self`.
        assert!(!out.contains("a.py:10-11"), "{out}");
        assert!(out.contains("method call, receiver type unknown"), "{out}");
    }

    #[test]
    fn cascade_follows_bash_functions_and_java_methods() {
        let dir = repo_with(
            "bash",
            &[("run.sh", "main() {\n  setup now\n  ls -l\n}\n\nsetup() {\n  echo hi\n}\n")],
        );
        let out = cascade_of(&dir, &["main()", "cascade:1"]);
        assert!(out.contains("Bash function: run.sh:6-8"), "{out}");

        let dir = repo_with(
            "java",
            &[(
                "A.java",
                "class A {\n  void run() {\n    helper();\n  }\n  void helper() {\n  }\n}\n",
            )],
        );
        let out = cascade_of(&dir, &["void run", "cascade:1"]);
        assert!(out.contains("Java method: A.java:5-6"), "{out}");
    }

    #[test]
    fn cascade_does_not_mix_languages_or_follow_into_test_files() {
        let dir = repo_with(
            "mix",
            &[
                ("a.py", "def main():\n    helper()\n"),
                ("b.go", "package p\n\nfunc helper() {}\n"),
                ("test_a.py", "def helper():\n    pass\n"),
            ],
        );
        let out = cascade_of(&dir, &["def main", "cascade:1"]);
        assert!(!out.contains("== CASCADE depth 1"), "{out}");
        assert!(out.contains("Not followed, external or std: 1 distinct names"), "{out}");
    }

    #[test]
    fn a_seed_in_a_test_file_or_a_plain_line_is_shown_but_not_followed() {
        let dir = repo_with(
            "seed",
            &[
                ("test_a.py", "def check():\n    helper()\n"),
                ("a.py", "def helper():\n    pass\n"),
            ],
        );
        let out = cascade_of(&dir, &["check", "cascade:1"]);
        assert!(out.contains("1 initial units not followed"), "{out}");
        assert!(!out.contains("== CASCADE depth 1"), "{out}");
    }

    #[test]
    fn a_function_called_from_many_units_is_a_utility_leaf() {
        let mut calls = String::new();
        for i in 0..8 {
            calls.push_str(&format!("def f{i}():\n    util()\n\n"));
        }
        let dir = repo_with(
            "util",
            &[
                ("a.py", &calls),
                ("u.py", "def util():\n    inner()\n\ndef inner():\n    pass\n"),
                ("m.py", "def main():\n    util()\n"),
            ],
        );
        let out = cascade_of(&dir, &["def main", "cascade:3"]);
        assert!(out.contains("utility, called from 9 units"), "{out}");
        // A utility is shown but not expanded.
        assert!(!out.contains("u.py:4-5"), "{out}");
    }

    #[test]
    fn rust_cascade_is_unaffected_by_other_languages_in_the_repository() {
        let rust = &[("src/lib.rs", "pub fn a() { b(); }\npub fn b() {}\n")];
        let alone = cascade_of(&repo_with("rs1", rust), &["fn a", "cascade:1"]);
        let mixed = cascade_of(
            &repo_with(
                "rs2",
                &[
                    rust[0],
                    ("b.py", "def b():\n    pass\n"),
                    ("lib.py", "def a():\n    pass\n"),
                ],
            ),
            &["fn a", "cascade:1"],
        );
        assert!(alone.contains("Rust function: src/lib.rs:2-2"), "{alone}");
        assert_eq!(alone, mixed);
    }

    #[test]
    fn extract_directive_parses_valid_depths() {
        let args = vec!["cascade:2".into(), "needle".into()];
        let (depth, rest) = extract_directive(&args).unwrap();
        assert_eq!(depth, Some(2));
        assert_eq!(rest, vec!["needle"]);

        let zero_args = vec!["needle".into(), "cascade:0".into()];
        let (z_depth, z_rest) = extract_directive(&zero_args).unwrap();
        assert_eq!(z_depth, Some(0));
        assert_eq!(z_rest, vec!["needle"]);
    }

    #[test]
    fn extract_directive_rejects_invalid_values() {
        for invalid in ["cascade:", "cascade:x", "cascade:-1", "cascade:6"] {
            let err = extract_directive(&[invalid.into(), "test".into()]).unwrap_err();
            assert!(err.contains("cascade: must be followed by a depth from 0 to 5"));
        }

        let multi = extract_directive(&["cascade:1".into(), "cascade:2".into()]);
        assert!(multi.unwrap_err().contains("only one cascade: directive"));

        let neg = extract_directive(&["-cascade:2".into()]);
        assert!(neg.unwrap_err().contains("negated selector"));

        let conflict_callers = extract_directive(&["cascade:1".into(), "callers:foo".into()]);
        assert!(
            conflict_callers
                .unwrap_err()
                .contains("cannot be combined with callers:")
        );

        let conflict_lines = extract_directive(&["cascade:1".into(), "lines:1-10".into()]);
        assert!(
            conflict_lines
                .unwrap_err()
                .contains("cannot be combined with lines:")
        );

        let lone_file = extract_directive(&["cascade:1".into(), "file:foo.rs".into()]);
        assert!(
            lone_file
                .unwrap_err()
                .contains("cannot be used with whole-file capture")
        );
    }
}
