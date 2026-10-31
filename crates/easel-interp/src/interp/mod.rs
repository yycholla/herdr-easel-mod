//! Evaluates a GPUI snippet into an element tree.
//!
//! This is a small tree-walking evaluator over `syn` syntax trees. It runs no
//! user code natively: anything it doesn't model is reported as a
//! [`Diagnostic`] instead, and evaluation continues so that every problem in
//! the snippet is reported at once.

mod builtins;
mod format;
mod macros;

use crate::diagnostic::{Diagnostic, DiagnosticKind, SourceMap};
use crate::ir::{Color, Element, Length, Node};
use macros::Macros;
use quote::ToTokens;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use syn::{
    BinOp, Block, Expr, ExprClosure, FnArg, ImplItem, Item, Lit, Pat, Stmt, UnOp, visit::Visit,
};

const MAX_NODES: usize = 20_000;
const MAX_DEPTH: usize = 64;
const MAX_LIST: usize = 10_000;
const MAX_ITERATIONS: usize = 100_000;

/// The result of interpreting a snippet.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub root: Node,
    /// What was rendered, e.g. `impl Render for HelloWorld` or `fn card`.
    pub entry: String,
}

/// Interprets `source`: a bare element expression or block, one or more
/// functions, or a full `impl Render` program.
pub fn interpret(source: &str) -> Result<Program, Vec<Diagnostic>> {
    match syn::parse_file(source) {
        Ok(file) => {
            let macros = Macros::collect_file(&file);
            Interp::new(SourceMap::default(), &macros).run_file(&file)
        }
        Err(file_err) => {
            let block_source = format!("{{{source}\n}}");
            match syn::parse_str::<Block>(&block_source) {
                Ok(block) => {
                    let macros = Macros::collect_block(&block);
                    Interp::new(
                        SourceMap {
                            first_line_shift: 1,
                        },
                        &macros,
                    )
                    .run_block(&block)
                }
                Err(block_err) => {
                    // Report the error from whichever reading the source resembles.
                    let looks_like_items = source.lines().map(str::trim_start).any(|line| {
                        [
                            "fn ", "pub ", "impl ", "struct ", "use ", "#[", "const ", "enum ",
                        ]
                        .iter()
                        .any(|prefix| line.starts_with(prefix))
                    });
                    let (err, map) = if looks_like_items {
                        (file_err, SourceMap::default())
                    } else {
                        (
                            block_err,
                            SourceMap {
                                first_line_shift: 1,
                            },
                        )
                    };
                    Err(vec![map.diagnostic_at(
                        DiagnosticKind::Syntax,
                        err.span(),
                        "",
                        err.to_string(),
                    )])
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Val<'a> {
    Element(Element),
    /// The receiver inside `.hover(|style| ..)`: collects `Styled` calls.
    Style(Vec<crate::ir::Call>),
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Color(Color),
    Length(Length),
    Variant(String, String),
    Tuple(Vec<Val<'a>>),
    List(Vec<Val<'a>>),
    Opt(Option<Box<Val<'a>>>),
    Struct(Rc<StructVal<'a>>),
    Closure(Rc<Closure<'a>>),
    /// A gpui `Window`/`App`/`Context` parameter. Using it needs compiled mode.
    Context(String),
    Unit,
    /// The result of an expression that already produced a diagnostic.
    Poison,
}

impl Val<'_> {
    fn describe(&self) -> String {
        match self {
            Val::Element(el) => format!("a `{}()` element", el.kind.name()),
            Val::Style(_) => "a style refinement".into(),
            Val::Str(_) => "a string".into(),
            Val::Int(_) => "an integer".into(),
            Val::Float(_) => "a float".into(),
            Val::Bool(_) => "a bool".into(),
            Val::Color(_) => "a color".into(),
            Val::Length(_) => "a length".into(),
            Val::Variant(ty, v) => format!("`{ty}::{v}`"),
            Val::Tuple(_) => "a tuple".into(),
            Val::List(_) => "a list".into(),
            Val::Opt(_) => "an Option".into(),
            Val::Struct(s) => format!("a `{}` value", s.ty),
            Val::Closure(_) => "a closure".into(),
            Val::Context(name) => format!("the gpui context `{name}`"),
            Val::Unit => "`()`".into(),
            Val::Poison => "an invalid value".into(),
        }
    }
}

#[derive(Debug)]
struct StructVal<'a> {
    ty: String,
    fields: Vec<(String, Val<'a>)>,
    /// False for a view whose construction wasn't found in the snippet.
    known: bool,
}

#[derive(Debug)]
struct Closure<'a> {
    expr: &'a ExprClosure,
    env: Env<'a>,
    self_ty: Option<String>,
}

/// A linked list of bindings. Closures capture it by cloning, and each
/// binding is a mutable cell, so assignment behaves like Rust's.
#[derive(Clone, Debug, Default)]
struct Env<'a>(Option<Rc<Binding<'a>>>);

#[derive(Debug)]
struct Binding<'a> {
    name: String,
    val: RefCell<Val<'a>>,
    next: Env<'a>,
}

impl<'a> Env<'a> {
    fn with(&self, name: impl Into<String>, val: Val<'a>) -> Self {
        Env(Some(Rc::new(Binding {
            name: name.into(),
            val: RefCell::new(val),
            next: self.clone(),
        })))
    }

    fn binding(&self, name: &str) -> Option<&Binding<'a>> {
        let mut env = self;
        while let Some(binding) = &env.0 {
            if binding.name == name {
                return Some(binding);
            }
            env = &binding.next;
        }
        None
    }

    fn get(&self, name: &str) -> Option<Val<'a>> {
        self.binding(name).map(|b| b.val.borrow().clone())
    }

    /// Replaces an existing binding's value. False if `name` isn't bound.
    fn set(&self, name: &str, val: Val<'a>) -> bool {
        match self.binding(name) {
            Some(binding) => {
                *binding.val.borrow_mut() = val;
                true
            }
            None => false,
        }
    }
}

#[derive(Clone, Copy)]
struct FnDef<'a> {
    inputs: &'a syn::punctuated::Punctuated<FnArg, syn::Token![,]>,
    block: &'a Block,
    /// The `impl` type for methods and associated functions.
    self_ty: Option<&'a syn::Type>,
}

impl FnDef<'_> {
    fn takes_self(&self) -> bool {
        matches!(self.inputs.first(), Some(FnArg::Receiver(_)))
    }
}

struct Interp<'a> {
    map: SourceMap,
    macros: &'a Macros,
    /// Free functions by name, methods and associated functions as `Type::name`.
    fns: HashMap<String, FnDef<'a>>,
    consts: HashMap<String, &'a Expr>,
    /// Types with an `impl RenderOnce`, usable directly as children.
    render_once: HashMap<String, FnDef<'a>>,
    type_names: Vec<String>,
    diagnostics: Vec<Diagnostic>,
    nodes: usize,
    depth: usize,
    /// The `impl` type of the function being evaluated, for `Self`.
    self_ty: Option<String>,
}

impl<'a> Interp<'a> {
    fn new(map: SourceMap, macros: &'a Macros) -> Self {
        Self {
            map,
            macros,
            fns: HashMap::new(),
            consts: HashMap::new(),
            render_once: HashMap::new(),
            type_names: Vec::new(),
            diagnostics: Vec::new(),
            nodes: 0,
            depth: 0,
            self_ty: None,
        }
    }

    fn finish(self, root: Val<'a>, entry: String) -> Result<Program, Vec<Diagnostic>> {
        let mut this = self;
        let root = match root {
            Val::Element(el) => Some(Node::Element(el)),
            Val::Str(text) => Some(Node::Text { text }),
            Val::Poison => None,
            other => {
                let message = format!(
                    "{entry} must return an element, but returned {}",
                    other.describe()
                );
                this.diagnostics.push(Diagnostic {
                    kind: DiagnosticKind::Invalid,
                    message,
                    line: 1,
                    column: 1,
                    snippet: String::new(),
                });
                None
            }
        };
        match root {
            Some(root) if this.diagnostics.is_empty() => Ok(Program { root, entry }),
            _ => Err(this.diagnostics),
        }
    }

    fn run_block(mut self, block: &'a Block) -> Result<Program, Vec<Diagnostic>> {
        let root = self.eval_block(block, &Env::default());
        self.finish(root, "the snippet".into())
    }

    fn run_file(mut self, file: &'a syn::File) -> Result<Program, Vec<Diagnostic>> {
        let mut render_impls = Vec::new();
        for item in &file.items {
            match item {
                Item::Fn(f) => {
                    self.fns.insert(
                        f.sig.ident.to_string(),
                        FnDef {
                            inputs: &f.sig.inputs,
                            block: &f.block,
                            self_ty: None,
                        },
                    );
                }
                Item::Const(c) => {
                    self.consts.insert(c.ident.to_string(), &c.expr);
                }
                Item::Static(s) => {
                    self.consts.insert(s.ident.to_string(), &s.expr);
                }
                Item::Struct(s) => self.type_names.push(s.ident.to_string()),
                Item::Enum(e) => self.type_names.push(e.ident.to_string()),
                Item::Impl(imp) => {
                    let Some(ty) = type_name(&imp.self_ty) else {
                        continue;
                    };
                    let trait_name = imp
                        .trait_
                        .as_ref()
                        .and_then(|(_, path, _)| path.segments.last())
                        .map(|seg| seg.ident.to_string());
                    for impl_item in &imp.items {
                        let ImplItem::Fn(method) = impl_item else {
                            continue;
                        };
                        let def = FnDef {
                            inputs: &method.sig.inputs,
                            block: &method.block,
                            self_ty: Some(&imp.self_ty),
                        };
                        let name = method.sig.ident.to_string();
                        match trait_name.as_deref() {
                            Some("Render") if name == "render" => {
                                render_impls.push((ty.clone(), def, false))
                            }
                            Some("RenderOnce") if name == "render" => {
                                self.render_once.insert(ty.to_string(), def);
                                render_impls.push((ty.clone(), def, true));
                            }
                            Some(_) => {}
                            None => {
                                self.fns.insert(format!("{ty}::{name}"), def);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Entry point: a `Render` view, then a conventionally named function,
        // then the only zero-argument function, then a lone `RenderOnce`.
        if let Some((ty, def, _)) = render_impls.iter().find(|(_, _, once)| !once) {
            return self.run_view(file, ty, *def, "Render");
        }
        const ENTRY_NAMES: &[&str] = &["render", "preview", "view", "ui", "app", "root"];
        let candidates: Vec<&syn::ItemFn> = file
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Fn(f) if f.sig.ident != "main" => Some(f),
                _ => None,
            })
            .collect();
        let entry_fn = candidates
            .iter()
            .find(|f| ENTRY_NAMES.contains(&f.sig.ident.to_string().as_str()))
            .or_else(|| {
                let zero_arg: Vec<_> = candidates
                    .iter()
                    .filter(|f| f.sig.inputs.is_empty())
                    .collect();
                (zero_arg.len() == 1).then(|| zero_arg[0])
            });
        if let Some(f) = entry_fn {
            let entry = format!("fn {}", f.sig.ident);
            let def = self.fns[&f.sig.ident.to_string()];
            let args = context_args(def);
            let root = self.call_fn(def, None, args, &f.sig.ident);
            return self.finish(root, entry);
        }
        if let [(ty, def, true)] = render_impls.as_slice() {
            return self.run_view(file, ty, *def, "RenderOnce");
        }

        let message = if candidates.is_empty() {
            "nothing to render: add an expression, a `fn render() -> impl IntoElement`, or an `impl Render`"
                .to_string()
        } else {
            let names: Vec<_> = candidates
                .iter()
                .map(|f| format!("`{}`", f.sig.ident))
                .collect();
            format!(
                "can't tell which function to render ({}); name the entry point `render` or `preview`",
                names.join(", ")
            )
        };
        self.diagnostics.push(Diagnostic {
            kind: DiagnosticKind::Invalid,
            message,
            line: 1,
            column: 1,
            snippet: String::new(),
        });
        Err(self.diagnostics)
    }

    /// Renders a view type, taking its field values from the first struct
    /// literal of that type in the file (usually in `main`).
    fn run_view(
        mut self,
        file: &'a syn::File,
        ty: &str,
        def: FnDef<'a>,
        trait_name: &str,
    ) -> Result<Program, Vec<Diagnostic>> {
        let entry = format!("impl {trait_name} for {ty}");
        let mut finder = StructLiteralFinder { ty, found: None };
        finder.visit_file(file);
        let this = match finder.found {
            Some(literal) => self.eval_struct(literal, &Env::default()),
            None => Val::Struct(Rc::new(StructVal {
                ty: ty.to_string(),
                fields: Vec::new(),
                known: false,
            })),
        };
        let root = self.call_fn(def, Some(this), context_args(def), def.block);
        self.finish(root, entry)
    }

    // ---- diagnostics ----

    fn report(
        &mut self,
        kind: DiagnosticKind,
        node: &dyn ToTokens,
        message: impl Into<String>,
    ) -> Val<'a> {
        let diagnostic = self.map.diagnostic(kind, node, message);
        self.diagnostics.push(diagnostic);
        Val::Poison
    }

    fn unsupported(&mut self, node: &dyn ToTokens, message: impl Into<String>) -> Val<'a> {
        self.report(DiagnosticKind::Unsupported, node, message)
    }

    fn invalid(&mut self, node: &dyn ToTokens, message: impl Into<String>) -> Val<'a> {
        self.report(DiagnosticKind::Invalid, node, message)
    }

    // ---- statements ----

    fn eval_block(&mut self, block: &'a Block, env: &Env<'a>) -> Val<'a> {
        let mut env = env.clone();
        let mut last = Val::Unit;
        for stmt in &block.stmts {
            last = Val::Unit;
            match stmt {
                Stmt::Local(local) => {
                    let Some(init) = &local.init else {
                        self.unsupported(local, "`let` without a value");
                        continue;
                    };
                    if init.diverge.is_some() {
                        self.unsupported(local, "`let ... else` isn't supported");
                        continue;
                    }
                    let val = self.eval(&init.expr, &env);
                    env = self.bind(&local.pat, val, env);
                }
                Stmt::Expr(expr, semi) => {
                    let val = self.eval(expr, &env);
                    if semi.is_none() {
                        last = val;
                    }
                }
                Stmt::Macro(stmt_macro) => {
                    let val = self.eval_macro(&stmt_macro.mac, &env);
                    if stmt_macro.semi_token.is_none() {
                        last = val;
                    }
                }
                // Nested items are hoisted, which only matters for name clashes.
                Stmt::Item(Item::Fn(f)) => {
                    let def = FnDef {
                        inputs: &f.sig.inputs,
                        block: &f.block,
                        self_ty: None,
                    };
                    self.fns.insert(f.sig.ident.to_string(), def);
                }
                Stmt::Item(Item::Const(c)) => {
                    self.consts.insert(c.ident.to_string(), &c.expr);
                }
                Stmt::Item(Item::Static(s)) => {
                    self.consts.insert(s.ident.to_string(), &s.expr);
                }
                Stmt::Item(Item::Use(_)) => {}
                Stmt::Item(item) => {
                    self.unsupported(item, "this kind of item isn't supported inside a block");
                }
            }
        }
        last
    }

    /// Assigns to a local (`x = ..`, `x += ..`) or a field of `self`.
    fn assign(&mut self, target: &'a Expr, val: Val<'a>, env: &Env<'a>) -> Val<'a> {
        match target {
            Expr::Path(path) if path.path.get_ident().is_some() => {
                let name = path.path.get_ident().unwrap().to_string();
                if !env.set(&name, val) {
                    return self.invalid(
                        target,
                        format!("cannot assign to `{name}`: not a local variable"),
                    );
                }
                Val::Unit
            }
            Expr::Field(field) if is_self(&field.base) => {
                let syn::Member::Named(name) = &field.member else {
                    return self.unsupported(target, "only named fields can be assigned");
                };
                let Some(Val::Struct(s)) = env.get("self") else {
                    return self.unsupported(target, "`self` isn't available here");
                };
                let mut fields = s.fields.clone();
                match fields.iter_mut().find(|(n, _)| name == n) {
                    Some(slot) => slot.1 = val,
                    None => fields.push((name.to_string(), val)),
                }
                let updated = StructVal {
                    ty: s.ty.clone(),
                    fields,
                    known: s.known,
                };
                env.set("self", Val::Struct(Rc::new(updated)));
                Val::Unit
            }
            _ => self.unsupported(target, "only locals and `self.field` can be assigned"),
        }
    }

    fn eval_for(&mut self, e: &'a syn::ExprForLoop, env: &Env<'a>) -> Val<'a> {
        let items = match self.eval(&e.expr, env) {
            Val::List(items) => items,
            Val::Opt(item) => item.map(|item| vec![*item]).unwrap_or_default(),
            Val::Poison => return Val::Poison,
            other => {
                let message = format!("can't iterate over {}", other.describe());
                return self.invalid(&e.expr, message);
            }
        };
        for item in items {
            let env = self.bind(&e.pat, item, env.clone());
            if matches!(self.eval_block(&e.body, &env), Val::Poison) && !self.diagnostics.is_empty()
            {
                break;
            }
        }
        Val::Unit
    }

    fn eval_while(&mut self, e: &'a syn::ExprWhile, env: &Env<'a>) -> Val<'a> {
        for _ in 0..MAX_ITERATIONS {
            match self.eval(&e.cond, env) {
                Val::Bool(true) => {
                    let errors = self.diagnostics.len();
                    self.eval_block(&e.body, env);
                    if self.diagnostics.len() > errors {
                        return Val::Poison;
                    }
                }
                Val::Bool(false) => return Val::Unit,
                Val::Poison => return Val::Poison,
                other => {
                    let message = format!(
                        "`while` condition must be a bool, found {}",
                        other.describe()
                    );
                    return self.invalid(&e.cond, message);
                }
            }
        }
        self.invalid(e, format!("loop ran more than {MAX_ITERATIONS} times"))
    }

    fn eval_match(&mut self, e: &'a syn::ExprMatch, env: &Env<'a>) -> Val<'a> {
        let scrutinee = self.eval(&e.expr, env);
        if matches!(scrutinee, Val::Poison) {
            return Val::Poison;
        }
        for arm in &e.arms {
            let Some(arm_env) = self.match_pat(&arm.pat, &scrutinee, env.clone()) else {
                continue;
            };
            if let Some((_, guard)) = &arm.guard {
                match self.eval(guard, &arm_env) {
                    Val::Bool(true) => {}
                    Val::Bool(false) => continue,
                    _ => return Val::Poison,
                }
            }
            return self.eval(&arm.body, &arm_env);
        }
        let message = format!("no `match` arm matches {}", scrutinee.describe());
        self.invalid(e, message)
    }

    /// The environment extended with the pattern's bindings, or `None` if
    /// `val` doesn't match.
    fn match_pat(&mut self, pat: &'a Pat, val: &Val<'a>, env: Env<'a>) -> Option<Env<'a>> {
        match pat {
            Pat::Wild(_) => Some(env),
            Pat::Ident(p)
                if p.subpat.is_none() && !self.consts.contains_key(&p.ident.to_string()) =>
            {
                Some(env.with(p.ident.to_string(), val.clone()))
            }
            Pat::Ident(p) => {
                let expected = self.eval_path_str(&p.ident.to_string(), pat, &env);
                values_equal(&expected, val).then_some(env)
            }
            Pat::Lit(lit) => {
                let expected = self.eval_lit(&lit.lit);
                values_equal(&expected, val).then_some(env)
            }
            Pat::Path(path) => {
                let expected = self.eval_path(path, &env);
                values_equal(&expected, val).then_some(env)
            }
            Pat::Or(or) => or
                .cases
                .iter()
                .find_map(|case| self.match_pat(case, val, env.clone())),
            Pat::Paren(p) => self.match_pat(&p.pat, val, env),
            Pat::Reference(p) => self.match_pat(&p.pat, val, env),
            Pat::Type(p) => self.match_pat(&p.pat, val, env),
            Pat::Tuple(t) => match val {
                Val::Tuple(items) if items.len() == t.elems.len() => t
                    .elems
                    .iter()
                    .zip(items)
                    .try_fold(env, |env, (p, v)| self.match_pat(p, v, env)),
                _ => None,
            },
            Pat::TupleStruct(ts) if ts.path.is_ident("Some") && ts.elems.len() == 1 => match val {
                Val::Opt(Some(inner)) => self.match_pat(&ts.elems[0], inner, env),
                _ => None,
            },
            Pat::Range(range) => {
                let bound = |this: &mut Self, e: &'a Option<Box<Expr>>| {
                    e.as_ref().map(|e| this.eval(e, &env))
                };
                let start = bound(self, &range.start);
                let end = bound(self, &range.end);
                let closed = matches!(range.limits, syn::RangeLimits::Closed(_));
                let in_range = match (val, start, end) {
                    (Val::Int(v), start, end) => {
                        let lo = match start {
                            Some(Val::Int(lo)) => *v >= lo,
                            None => true,
                            _ => false,
                        };
                        let hi = match end {
                            Some(Val::Int(hi)) => {
                                if closed {
                                    *v <= hi
                                } else {
                                    *v < hi
                                }
                            }
                            None => true,
                            _ => false,
                        };
                        lo && hi
                    }
                    _ => false,
                };
                in_range.then_some(env)
            }
            _ => {
                self.unsupported(pat, "this pattern isn't supported in `match`");
                None
            }
        }
    }

    fn eval_path_str(&mut self, name: &str, at: &dyn ToTokens, env: &Env<'a>) -> Val<'a> {
        if let Some(val) = env.get(name) {
            return val;
        }
        match self.consts.get(name).copied() {
            Some(expr) => self.eval(expr, &Env::default()),
            None => self.invalid(at, format!("cannot find `{name}` in this snippet")),
        }
    }

    fn bind(&mut self, pat: &'a Pat, val: Val<'a>, env: Env<'a>) -> Env<'a> {
        match pat {
            Pat::Ident(p) if p.subpat.is_none() => env.with(p.ident.to_string(), val),
            Pat::Type(p) => self.bind(&p.pat, val, env),
            Pat::Reference(p) => self.bind(&p.pat, val, env),
            Pat::Paren(p) => self.bind(&p.pat, val, env),
            Pat::Wild(_) => env,
            Pat::Tuple(t) => match val {
                Val::Tuple(items) if items.len() == t.elems.len() => t
                    .elems
                    .iter()
                    .zip(items)
                    .fold(env, |env, (p, v)| self.bind(p, v, env)),
                Val::Poison => t
                    .elems
                    .iter()
                    .fold(env, |env, p| self.bind(p, Val::Poison, env)),
                other => {
                    let message = format!(
                        "expected a {}-tuple, found {}",
                        t.elems.len(),
                        other.describe()
                    );
                    self.invalid(pat, message);
                    t.elems
                        .iter()
                        .fold(env, |env, p| self.bind(p, Val::Poison, env))
                }
            },
            _ => {
                self.unsupported(pat, "this pattern isn't supported; use a name or a tuple");
                env
            }
        }
    }

    // ---- functions ----

    fn call_fn(
        &mut self,
        def: FnDef<'a>,
        this: Option<Val<'a>>,
        args: Vec<Val<'a>>,
        at: &dyn ToTokens,
    ) -> Val<'a> {
        let params: Vec<&'a Pat> = def
            .inputs
            .iter()
            .filter_map(|input| match input {
                FnArg::Typed(typed) => Some(&*typed.pat),
                FnArg::Receiver(_) => None,
            })
            .collect();
        if params.len() != args.len() {
            let message = format!(
                "expected {} argument(s), found {}",
                params.len(),
                args.len()
            );
            return self.invalid(at, message);
        }
        if self.depth >= MAX_DEPTH {
            return self.invalid(
                at,
                format!("calls nested deeper than {MAX_DEPTH}; is this recursive?"),
            );
        }
        let mut env = Env::default();
        if let Some(this) = this {
            env = env.with("self", this);
        }
        for (pat, arg) in params.into_iter().zip(args) {
            env = self.bind(pat, arg, env);
        }
        self.depth += 1;
        let saved = std::mem::replace(&mut self.self_ty, def.self_ty.and_then(type_name));
        let result = self.eval_block(def.block, &env);
        self.self_ty = saved;
        self.depth -= 1;
        result
    }

    fn call_closure(
        &mut self,
        closure: &Closure<'a>,
        args: Vec<Val<'a>>,
        at: &dyn ToTokens,
    ) -> Val<'a> {
        let inputs = &closure.expr.inputs;
        if inputs.len() != args.len() {
            let message = format!(
                "this closure takes {} argument(s) but is given {}",
                inputs.len(),
                args.len()
            );
            return self.invalid(at, message);
        }
        if self.depth >= MAX_DEPTH {
            return self.invalid(at, format!("calls nested deeper than {MAX_DEPTH}"));
        }
        let mut env = closure.env.clone();
        for (pat, arg) in inputs.iter().zip(args) {
            env = self.bind(pat, arg, env);
        }
        self.depth += 1;
        let saved = std::mem::replace(&mut self.self_ty, closure.self_ty.clone());
        let result = self.eval(&closure.expr.body, &env);
        self.self_ty = saved;
        self.depth -= 1;
        result
    }

    // ---- expressions ----

    fn eval(&mut self, expr: &'a Expr, env: &Env<'a>) -> Val<'a> {
        match expr {
            Expr::Lit(lit) => self.eval_lit(&lit.lit),
            Expr::Path(path) => self.eval_path(path, env),
            Expr::Call(call) => self.eval_call(call, env),
            Expr::MethodCall(call) => self.eval_method_call(call, env),
            Expr::Macro(mac) => self.eval_macro(&mac.mac, env),
            Expr::Paren(e) => self.eval(&e.expr, env),
            Expr::Group(e) => self.eval(&e.expr, env),
            Expr::Reference(e) => self.eval(&e.expr, env),
            Expr::Block(e) => self.eval_block(&e.block, env),
            Expr::Unary(e) => self.eval_unary(e, env),
            Expr::Binary(e) => self.eval_binary(e, env),
            Expr::If(e) => self.eval_if(e, env),
            Expr::Array(e) => {
                let items = e.elems.iter().map(|item| self.eval(item, env)).collect();
                Val::List(items)
            }
            Expr::Tuple(e) if e.elems.is_empty() => Val::Unit,
            Expr::Tuple(e) => Val::Tuple(e.elems.iter().map(|item| self.eval(item, env)).collect()),
            Expr::Field(e) => self.eval_field(e, env),
            Expr::Index(e) => self.eval_index(e, env),
            Expr::Cast(e) => self.eval_cast(e, env),
            Expr::Range(e) => self.eval_range(e, env),
            Expr::Struct(e) => self.eval_struct(e, env),
            Expr::Closure(e) => Val::Closure(Rc::new(Closure {
                expr: e,
                env: env.clone(),
                self_ty: self.self_ty.clone(),
            })),
            Expr::Match(e) => self.eval_match(e, env),
            Expr::ForLoop(e) => self.eval_for(e, env),
            Expr::While(e) if !matches!(*e.cond, Expr::Let(_)) => self.eval_while(e, env),
            Expr::Assign(e) => {
                let val = self.eval(&e.right, env);
                self.assign(&e.left, val, env)
            }
            Expr::Loop(_) | Expr::While(_) | Expr::Break(_) | Expr::Continue(_) => self.unsupported(
                expr,
                "`loop`, `while let`, `break` and `continue` aren't supported; use `for`, `while` or iterators",
            ),
            Expr::Return(_) | Expr::Try(_) => self.unsupported(expr, "early returns aren't supported"),
            _ => self.unsupported(expr, "this kind of expression isn't supported"),
        }
    }

    fn eval_lit(&mut self, lit: &'a Lit) -> Val<'a> {
        match lit {
            Lit::Str(s) => Val::Str(s.value()),
            Lit::Char(c) => Val::Str(c.value().to_string()),
            Lit::Bool(b) => Val::Bool(b.value),
            Lit::Int(i) if matches!(i.suffix(), "f32" | "f64") => {
                Val::Float(i.base10_parse::<f64>().unwrap_or_default())
            }
            Lit::Int(i) => match i.base10_parse::<i64>() {
                Ok(n) => Val::Int(n),
                Err(_) => self.invalid(lit, "integer literal out of range"),
            },
            Lit::Float(f) => match f.base10_parse::<f64>() {
                Ok(n) => Val::Float(n),
                Err(_) => self.invalid(lit, "invalid float literal"),
            },
            _ => self.unsupported(lit, "this literal isn't supported"),
        }
    }

    fn eval_path(&mut self, path: &'a syn::ExprPath, env: &Env<'a>) -> Val<'a> {
        if path.qself.is_some() {
            return self.unsupported(path, "qualified paths aren't supported");
        }
        let segments = path_segments(&path.path);
        match segments.as_slice() {
            [name] => {
                if let Some(val) = env.get(name) {
                    return val;
                }
                if name == "None" {
                    return Val::Opt(None);
                }
                if name == "self" {
                    return self.unsupported(path, "`self` isn't available here");
                }
                if let Some(expr) = self.consts.get(name.as_str()).copied() {
                    if self.depth >= MAX_DEPTH {
                        return self.invalid(path, "constant refers to itself");
                    }
                    self.depth += 1;
                    let val = self.eval(expr, &Env::default());
                    self.depth -= 1;
                    return val;
                }
                if matches!(name.as_str(), "cx" | "_cx" | "window" | "_window") {
                    return self.unsupported(
                        path,
                        format!(
                            "`{name}` is a live gpui context; code that uses it needs compiled mode"
                        ),
                    );
                }
                self.invalid(path, format!("cannot find `{name}` in this snippet"))
            }
            [ty, variant] if ty.starts_with(char::is_uppercase) => {
                let ty = if ty == "Self" {
                    self.self_ty.clone().unwrap_or_default()
                } else {
                    ty.clone()
                };
                Val::Variant(ty, variant.clone())
            }
            _ => self.unsupported(path, "this path isn't supported"),
        }
    }

    fn eval_unary(&mut self, e: &'a syn::ExprUnary, env: &Env<'a>) -> Val<'a> {
        let val = self.eval(&e.expr, env);
        match (&e.op, val) {
            (_, Val::Poison) => Val::Poison,
            (UnOp::Deref(_), val) => val,
            (UnOp::Neg(_), Val::Int(n)) => Val::Int(-n),
            (UnOp::Neg(_), Val::Float(n)) => Val::Float(-n),
            (UnOp::Neg(_), Val::Length(len)) => Val::Length(scale_length(len, -1.0)),
            (UnOp::Not(_), Val::Bool(b)) => Val::Bool(!b),
            (_, other) => {
                let message = format!(
                    "can't apply `{}` to {}",
                    e.op.to_token_stream(),
                    other.describe()
                );
                self.invalid(e, message)
            }
        }
    }

    fn eval_binary(&mut self, e: &'a syn::ExprBinary, env: &Env<'a>) -> Val<'a> {
        if let Some(op) = compound_base(&e.op) {
            let current = self.eval(&e.left, env);
            let right = self.eval(&e.right, env);
            let val = self.binary_values(&op, current, right, e);
            return self.assign(&e.left, val, env);
        }
        let left = self.eval(&e.left, env);
        // Short-circuit `&&` and `||` like Rust does.
        match (&e.op, &left) {
            (BinOp::And(_), Val::Bool(false)) => return Val::Bool(false),
            (BinOp::Or(_), Val::Bool(true)) => return Val::Bool(true),
            _ => {}
        }
        let right = self.eval(&e.right, env);
        self.binary_values(&e.op, left, right, e)
    }

    fn binary_values(
        &mut self,
        op: &BinOp,
        left: Val<'a>,
        right: Val<'a>,
        e: &'a syn::ExprBinary,
    ) -> Val<'a> {
        use Val::*;
        let result = match (op, left, right) {
            (_, Poison, _) | (_, _, Poison) => Some(Poison),
            (BinOp::And(_) | BinOp::Or(_), Bool(_), Bool(b)) => Some(Bool(b)),
            (op, Int(a), Int(b)) => int_op(op, a, b),
            (op, Float(a), Float(b)) => float_op(op, a, b),
            (BinOp::Add(_), Str(a), Str(b)) => Some(Str(a + &b)),
            (BinOp::Eq(_), Str(a), Str(b)) => Some(Bool(a == b)),
            (BinOp::Ne(_), Str(a), Str(b)) => Some(Bool(a != b)),
            (BinOp::Eq(_), Bool(a), Bool(b)) => Some(Bool(a == b)),
            (BinOp::Ne(_), Bool(a), Bool(b)) => Some(Bool(a != b)),
            (BinOp::Eq(_), a @ Variant(..), b @ Variant(..)) => Some(Bool(values_equal(&a, &b))),
            (BinOp::Ne(_), a @ Variant(..), b @ Variant(..)) => Some(Bool(!values_equal(&a, &b))),
            (BinOp::Mul(_), Length(len), Float(f)) | (BinOp::Mul(_), Float(f), Length(len)) => {
                Some(Length(scale_length(len, f)))
            }
            (BinOp::Div(_), Length(len), Float(f)) => Some(Length(scale_length(len, 1.0 / f))),
            (op @ (BinOp::Add(_) | BinOp::Sub(_)), Length(a), Length(b)) => {
                add_lengths(op, a, b).map(Length)
            }
            (_, left, right) => {
                let message = format!(
                    "can't apply `{}` to {} and {}",
                    op.to_token_stream(),
                    left.describe(),
                    right.describe()
                );
                return self.invalid(e, message);
            }
        };
        match result {
            Some(val) => val,
            None => self.invalid(
                e,
                format!(
                    "`{}` isn't supported here (or overflowed)",
                    op.to_token_stream()
                ),
            ),
        }
    }

    fn eval_if(&mut self, e: &'a syn::ExprIf, env: &Env<'a>) -> Val<'a> {
        match self.eval(&e.cond, env) {
            Val::Bool(true) => self.eval_block(&e.then_branch, env),
            Val::Bool(false) => match &e.else_branch {
                Some((_, else_branch)) => self.eval(else_branch, env),
                None => Val::Unit,
            },
            Val::Poison => Val::Poison,
            other => {
                let message = format!("`if` condition must be a bool, found {}", other.describe());
                self.invalid(&e.cond, message)
            }
        }
    }

    fn eval_field(&mut self, e: &'a syn::ExprField, env: &Env<'a>) -> Val<'a> {
        let base = self.eval(&e.base, env);
        match (base, &e.member) {
            (Val::Poison, _) => Val::Poison,
            (Val::Tuple(items), syn::Member::Unnamed(index)) => {
                match items.get(index.index as usize) {
                    Some(val) => val.clone(),
                    None => self.invalid(e, "tuple index out of range"),
                }
            }
            (Val::Struct(s), syn::Member::Named(name)) => {
                match s.fields.iter().find(|(n, _)| name == n) {
                    Some((_, val)) => val.clone(),
                    None if !s.known => self.unsupported(
                        e,
                        format!(
                            "can't see the value of `{}`: construct the view in the snippet \
                             (e.g. `{} {{ .. }}` in `main`) or use compiled mode",
                            e.to_token_stream(),
                            s.ty
                        ),
                    ),
                    None => self.invalid(e, format!("`{}` has no field `{name}`", s.ty)),
                }
            }
            (other, member) => {
                let message = format!(
                    "{} has no field `{}`",
                    other.describe(),
                    member.to_token_stream()
                );
                self.invalid(e, message)
            }
        }
    }

    fn eval_index(&mut self, e: &'a syn::ExprIndex, env: &Env<'a>) -> Val<'a> {
        let base = self.eval(&e.expr, env);
        let index = self.eval(&e.index, env);
        match (base, index) {
            (Val::Poison, _) | (_, Val::Poison) => Val::Poison,
            (Val::List(items), Val::Int(i)) => {
                match usize::try_from(i).ok().and_then(|i| items.get(i)) {
                    Some(val) => val.clone(),
                    None => self.invalid(
                        e,
                        format!("index {i} is out of bounds (len {})", items.len()),
                    ),
                }
            }
            (base, index) => {
                let message = format!("can't index {} with {}", base.describe(), index.describe());
                self.invalid(e, message)
            }
        }
    }

    fn eval_cast(&mut self, e: &'a syn::ExprCast, env: &Env<'a>) -> Val<'a> {
        let val = self.eval(&e.expr, env);
        let ty = e.ty.to_token_stream().to_string();
        let float = matches!(ty.as_str(), "f32" | "f64");
        let int = matches!(
            ty.as_str(),
            "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize"
        );
        match val {
            Val::Int(n) if float => Val::Float(n as f64),
            Val::Float(n) if int => Val::Int(n as i64),
            Val::Int(_) | Val::Float(_) if float || int => val,
            Val::Poison => Val::Poison,
            other => {
                let message = format!("can't cast {} to `{ty}`", other.describe());
                self.invalid(e, message)
            }
        }
    }

    fn eval_range(&mut self, e: &'a syn::ExprRange, env: &Env<'a>) -> Val<'a> {
        let (Some(start), Some(end)) = (&e.start, &e.end) else {
            return self.unsupported(e, "open-ended ranges aren't supported");
        };
        let (start, end) = (self.eval(start, env), self.eval(end, env));
        match (start, end) {
            (Val::Int(start), Val::Int(end)) => {
                let end = if matches!(e.limits, syn::RangeLimits::Closed(_)) {
                    end + 1
                } else {
                    end
                };
                if end - start > MAX_LIST as i64 {
                    return self.invalid(e, format!("ranges are limited to {MAX_LIST} items"));
                }
                Val::List((start..end).map(Val::Int).collect())
            }
            (Val::Poison, _) | (_, Val::Poison) => Val::Poison,
            (start, end) => {
                let message = format!(
                    "range bounds must be integers, found {} and {}",
                    start.describe(),
                    end.describe()
                );
                self.invalid(e, message)
            }
        }
    }

    fn eval_struct(&mut self, e: &'a syn::ExprStruct, env: &Env<'a>) -> Val<'a> {
        if e.rest.is_some() || e.dot2_token.is_some() {
            return self.unsupported(e, "struct update syntax (`..`) isn't supported");
        }
        let mut ty = path_segments(&e.path).pop().unwrap_or_default();
        if ty == "Self" {
            ty = self.self_ty.clone().unwrap_or(ty);
        }
        let mut fields: Vec<(String, Val<'a>)> = Vec::new();
        for field in &e.fields {
            let syn::Member::Named(name) = &field.member else {
                return self.unsupported(e, "tuple structs aren't supported");
            };
            let val = self.eval(&field.expr, env);
            fields.push((name.to_string(), val));
        }
        if ty == "Hsla" {
            let get = |name: &str| fields.iter().find(|(n, _)| n == name).map(|(_, v)| v);
            if let (
                Some(Val::Float(h)),
                Some(Val::Float(s)),
                Some(Val::Float(l)),
                Some(Val::Float(a)),
            ) = (get("h"), get("s"), get("l"), get("a"))
            {
                let (h, s, l, a) = (*h as f32, *s as f32, *l as f32, *a as f32);
                return Val::Color(Color::Hsla { h, s, l, a });
            }
            return self.invalid(e, "`Hsla { .. }` needs f32 fields `h`, `s`, `l` and `a`");
        }
        Val::Struct(Rc::new(StructVal {
            ty,
            fields,
            known: true,
        }))
    }

    // ---- elements ----

    /// Converts a value passed to `.child(..)` into a node, or `None` for an
    /// absent optional child.
    fn child_node(&mut self, val: Val<'a>, at: &dyn ToTokens) -> Option<Node> {
        match val {
            Val::Element(el) => Some(Node::Element(el)),
            Val::Str(text) => {
                self.count_node(at)?;
                Some(Node::Text { text })
            }
            Val::Opt(None) | Val::Poison => None,
            Val::Opt(Some(val)) => self.child_node(*val, at),
            Val::Struct(s) => {
                let Some(def) = self.render_once.get(&s.ty).copied() else {
                    let message = if self.type_names.contains(&s.ty) {
                        format!(
                            "`{}` isn't an element: implement `RenderOnce` for it, or call a function \
                             returning `impl IntoElement` (`Render` views need `cx.new`, i.e. compiled mode)",
                            s.ty
                        )
                    } else {
                        format!("`{}` isn't an element", s.ty)
                    };
                    self.invalid(at, message);
                    return None;
                };
                let rendered = self.call_fn(def, Some(Val::Struct(s)), context_args(def), at);
                self.child_node(rendered, at)
            }
            Val::Int(_) | Val::Float(_) => {
                self.invalid(
                    at,
                    "numbers aren't elements; use `format!(\"{}\", n)` or `n.to_string()`",
                );
                None
            }
            other => {
                let message = format!("{} isn't an element", other.describe());
                self.invalid(at, message);
                None
            }
        }
    }

    /// Counts a new element against the size budget.
    fn count_node(&mut self, at: &dyn ToTokens) -> Option<()> {
        self.nodes += 1;
        if self.nodes == MAX_NODES + 1 {
            self.invalid(
                at,
                format!("the snippet builds more than {MAX_NODES} elements"),
            );
        }
        (self.nodes <= MAX_NODES).then_some(())
    }

    fn new_element(&mut self, kind: crate::ir::ElementKind, at: &dyn ToTokens) -> Val<'a> {
        if self.count_node(at).is_none() {
            return Val::Poison;
        }
        let span = at.to_token_stream().into_iter().next().map(|t| t.span());
        let mut element = Element::new(kind);
        element.source = span.map(|span| {
            let (line, column) = self.map.locate(span);
            crate::ir::SourceRef { line, column }
        });
        Val::Element(element)
    }
}

fn is_self(expr: &Expr) -> bool {
    matches!(expr, Expr::Path(p) if p.path.is_ident("self"))
}

/// Path segments without a leading `gpui::`.
fn path_segments(path: &syn::Path) -> Vec<String> {
    let mut segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    if segments.len() > 1 && segments[0] == "gpui" {
        segments.remove(0);
    }
    segments
}

fn type_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

/// Placeholder arguments for a render function's `window`/`cx` parameters.
fn context_args<'a>(def: FnDef<'a>) -> Vec<Val<'a>> {
    def.inputs
        .iter()
        .filter_map(|input| match input {
            FnArg::Typed(typed) => Some(match &*typed.pat {
                Pat::Ident(ident) => Val::Context(ident.ident.to_string()),
                _ => Val::Context("cx".into()),
            }),
            FnArg::Receiver(_) => None,
        })
        .collect()
}

/// The plain operator behind a compound assignment like `+=`.
fn compound_base(op: &BinOp) -> Option<BinOp> {
    let span = proc_macro2::Span::call_site();
    Some(match op {
        BinOp::AddAssign(_) => BinOp::Add(syn::Token![+](span)),
        BinOp::SubAssign(_) => BinOp::Sub(syn::Token![-](span)),
        BinOp::MulAssign(_) => BinOp::Mul(syn::Token![*](span)),
        BinOp::DivAssign(_) => BinOp::Div(syn::Token![/](span)),
        BinOp::RemAssign(_) => BinOp::Rem(syn::Token![%](span)),
        _ => return None,
    })
}

/// `==` for values that support it; anything else compares unequal.
fn values_equal(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Str(a), Val::Str(b)) => a == b,
        (Val::Int(a), Val::Int(b)) => a == b,
        (Val::Float(a), Val::Float(b)) => a == b,
        (Val::Bool(a), Val::Bool(b)) => a == b,
        (Val::Unit, Val::Unit) => true,
        (Val::Opt(None), Val::Opt(None)) => true,
        (Val::Opt(Some(a)), Val::Opt(Some(b))) => values_equal(a, b),
        (Val::Variant(t1, v1), Val::Variant(t2, v2)) => {
            v1 == v2 && (t1 == t2 || t1.is_empty() || t2.is_empty())
        }
        (Val::Tuple(a), Val::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| values_equal(a, b))
        }
        _ => false,
    }
}

fn int_op<'a>(op: &BinOp, a: i64, b: i64) -> Option<Val<'a>> {
    Some(match op {
        BinOp::Add(_) => Val::Int(a.checked_add(b)?),
        BinOp::Sub(_) => Val::Int(a.checked_sub(b)?),
        BinOp::Mul(_) => Val::Int(a.checked_mul(b)?),
        BinOp::Div(_) => Val::Int(a.checked_div(b)?),
        BinOp::Rem(_) => Val::Int(a.checked_rem(b)?),
        _ => Val::Bool(compare(op, a.cmp(&b))?),
    })
}

fn float_op<'a>(op: &BinOp, a: f64, b: f64) -> Option<Val<'a>> {
    Some(match op {
        BinOp::Add(_) => Val::Float(a + b),
        BinOp::Sub(_) => Val::Float(a - b),
        BinOp::Mul(_) => Val::Float(a * b),
        BinOp::Div(_) => Val::Float(a / b),
        BinOp::Rem(_) => Val::Float(a % b),
        _ => Val::Bool(compare(op, a.partial_cmp(&b)?)?),
    })
}

fn compare(op: &BinOp, ordering: std::cmp::Ordering) -> Option<bool> {
    use std::cmp::Ordering::*;
    Some(match op {
        BinOp::Eq(_) => ordering == Equal,
        BinOp::Ne(_) => ordering != Equal,
        BinOp::Lt(_) => ordering == Less,
        BinOp::Le(_) => ordering != Greater,
        BinOp::Gt(_) => ordering == Greater,
        BinOp::Ge(_) => ordering != Less,
        _ => return None,
    })
}

fn scale_length(len: Length, factor: f64) -> Length {
    let factor = factor as f32;
    match len {
        Length::Px(v) => Length::Px(v * factor),
        Length::Rems(v) => Length::Rems(v * factor),
        Length::Relative(v) => Length::Relative(v * factor),
        Length::Auto => Length::Auto,
    }
}

fn add_lengths(op: &BinOp, a: Length, b: Length) -> Option<Length> {
    let sign = if matches!(op, BinOp::Sub(_)) {
        -1.0
    } else {
        1.0
    };
    Some(match (a, b) {
        (Length::Px(a), Length::Px(b)) => Length::Px(a + sign * b),
        (Length::Rems(a), Length::Rems(b)) => Length::Rems(a + sign * b),
        _ => return None,
    })
}

/// Finds the first `Ty { .. }` literal, i.e. how `main` constructs the view.
struct StructLiteralFinder<'a, 'f> {
    ty: &'f str,
    found: Option<&'a syn::ExprStruct>,
}

impl<'a> Visit<'a> for StructLiteralFinder<'a, '_> {
    fn visit_expr_struct(&mut self, node: &'a syn::ExprStruct) {
        if self.found.is_none()
            && node
                .path
                .segments
                .last()
                .is_some_and(|s| s.ident == self.ty)
        {
            self.found = Some(node);
        }
        syn::visit::visit_expr_struct(self, node);
    }
}
