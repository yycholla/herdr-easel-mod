//! Calls: gpui constructors and helpers, user functions, macros, and methods
//! on elements, strings, numbers, lists and options.

use super::format::{self, ArgRef, Piece};
use super::macros::MacroBody;
use super::{Env, Interp, MAX_LIST, Val, path_segments};
use crate::diagnostic::DiagnosticKind;
use crate::ir::{Call, Element, ElementKind, Length, Value};
use crate::styled::{self, ArgKind};
use quote::ToTokens;
use syn::{Expr, punctuated::Punctuated};

/// Methods gpui has that need a live window, focus or event loop.
const INTERACTIVE_METHODS: &[&str] = &[
    "tooltip",
    "hoverable_tooltip",
    "track_focus",
    "key_context",
    "focusable",
    "track_scroll",
    "tab_index",
    "tab_stop",
    "tab_group",
    "focus",
    "in_focus",
    "drag_over",
    "group_drag_over",
    "can_drop",
    "anchor_scroll",
    "occlude",
    "block_mouse_except_scroll",
    "window_control_area",
    "debug_selector",
    "with_animation",
    "with_fallback",
    "with_loading",
    "image_cache",
];

const NAMED_COLORS: &[&str] = &[
    "red",
    "green",
    "blue",
    "yellow",
    "black",
    "white",
    "transparent_black",
    "transparent_white",
];

impl<'a> Interp<'a> {
    pub(super) fn eval_call(&mut self, call: &'a syn::ExprCall, env: &Env<'a>) -> Val<'a> {
        let Expr::Path(func) = &*call.func else {
            let callee = self.eval(&call.func, env);
            let args = self.eval_args(&call.args, env);
            return self.call_value(callee, args, call);
        };
        let segments = path_segments(&func.path);
        let mut name = segments.join("::");
        if let [ty, method] = segments.as_slice()
            && ty == "Self"
        {
            name = format!("{}::{method}", self.self_ty.clone().unwrap_or_default());
        }

        // Locals holding closures shadow everything else.
        if let [local] = segments.as_slice()
            && let Some(callee) = env.get(local)
        {
            let args = self.eval_args(&call.args, env);
            return self.call_value(callee, args, call);
        }
        let args = self.eval_args(&call.args, env);
        if let Some(def) = self.fns.get(&name).copied() {
            if def.takes_self() {
                return self.invalid(
                    call,
                    format!(
                        "`{name}` is a method; call it as `value.{}()`",
                        segments.last().unwrap()
                    ),
                );
            }
            return self.call_fn(def, None, args, call);
        }
        if args.iter().any(|arg| matches!(arg, Val::Poison)) {
            return Val::Poison;
        }
        self.call_builtin(&name, args, call)
    }

    fn eval_args(
        &mut self,
        args: &'a Punctuated<Expr, syn::Token![,]>,
        env: &Env<'a>,
    ) -> Vec<Val<'a>> {
        args.iter().map(|arg| self.eval(arg, env)).collect()
    }

    fn call_value(&mut self, callee: Val<'a>, args: Vec<Val<'a>>, at: &dyn ToTokens) -> Val<'a> {
        match callee {
            Val::Closure(closure) => self.call_closure(&closure, args, at),
            Val::Poison => Val::Poison,
            other => {
                let message = format!("{} isn't callable", other.describe());
                self.invalid(at, message)
            }
        }
    }

    fn call_builtin(&mut self, name: &str, args: Vec<Val<'a>>, at: &'a syn::ExprCall) -> Val<'a> {
        use Val::*;
        match (name, args.as_slice()) {
            ("div", []) => self.new_element(ElementKind::Div, at),
            ("svg", []) => self.new_element(ElementKind::Svg, at),
            ("img", [Str(source)]) => self.new_element(ElementKind::Img { source: source.clone() }, at),
            ("rgb", [Int(hex)]) if (0..=0xFF_FFFF).contains(hex) => Color(crate::ir::Color::Rgb(*hex as u32)),
            ("rgba", [Int(hex)]) if (0..=0xFFFF_FFFF).contains(hex) => Color(crate::ir::Color::Rgba(*hex as u32)),
            ("hsla", [Float(h), Float(s), Float(l), Float(a)]) => Color(crate::ir::Color::Hsla {
                h: *h as f32,
                s: *s as f32,
                l: *l as f32,
                a: *a as f32,
            }),
            ("opaque_grey", [Float(l), Float(a)]) => Color(crate::ir::Color::Hsla {
                h: 0.,
                s: 0.,
                l: *l as f32,
                a: *a as f32,
            }),
            (color, []) if NAMED_COLORS.contains(&color) || color.strip_prefix("Hsla::").is_some_and(|c| NAMED_COLORS.contains(&c)) => {
                Color(crate::ir::Color::Named(color.trim_start_matches("Hsla::").to_string()))
            }
            ("Hsla::from", [Color(color)]) => Color(crate::ir::Color::Opacity {
                base: Box::new(color.clone()),
                factor: 1.,
            }),
            ("point", [x @ Length(_), y @ Length(_)]) => Struct(std::rc::Rc::new(super::StructVal {
                ty: "Point".into(),
                fields: vec![("x".into(), x.clone()), ("y".into(), y.clone())],
                known: true,
            })),
            ("px", [Float(v)]) => Length(crate::ir::Length::Px(*v as f32)),
            ("rems", [Float(v)]) => Length(crate::ir::Length::Rems(*v as f32)),
            ("relative", [Float(v)]) => Length(crate::ir::Length::Relative(*v as f32)),
            ("auto", []) => Length(crate::ir::Length::Auto),
            ("Some", [val]) => Opt(Some(Box::new(val.clone()))),
            ("String::new", []) => Str(String::new()),
            ("String::from" | "SharedString::from" | "SharedString::new" | "SharedString::new_static", [Str(s)]) => {
                Str(s.clone())
            }
            ("px" | "rems" | "relative" | "hsla" | "opaque_grey", _) if args.iter().any(|a| matches!(a, Int(_))) => {
                self.invalid(at, format!("`{name}` takes f32 arguments; write e.g. `{name}(4.)` instead of `{name}(4)`"))
            }
            ("rgb" | "rgba", [Float(_)]) => self.invalid(at, format!("`{name}` takes a hex integer like `{name}(0x3b82f6)`")),
            ("rgb", [Int(_)]) => self.invalid(at, "`rgb` takes 0xRRGGBB; use `rgba` for 0xRRGGBBAA"),
            ("div" | "svg" | "img" | "rgb" | "rgba" | "hsla" | "px" | "rems" | "relative" | "auto" | "Some", _) => {
                let described: Vec<_> = args.iter().map(Val::describe).collect();
                self.invalid(at, format!("unexpected arguments to `{name}`: [{}]", described.join(", ")))
            }
            ("linear_gradient" | "linear_color_stop", _) => {
                self.unsupported(at, "gradients aren't supported by the interpreter yet")
            }
            (_, _) if name.starts_with("cx::") || name.contains("Context") => {
                self.unsupported(at, format!("`{name}` needs a live gpui context; use compiled mode"))
            }
            _ => self.unsupported(at, format!("`{name}(..)` isn't available to the interpreter")),
        }
    }

    pub(super) fn eval_macro(&mut self, mac: &'a syn::Macro, env: &Env<'a>) -> Val<'a> {
        let name = mac
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        let body = self.macros.get(mac);
        match (name.as_str(), body) {
            (_, Some(MacroBody::Error(err))) => {
                let message = format!("can't parse `{name}!`: {err}");
                self.report(DiagnosticKind::Syntax, mac, message)
            }
            ("format", Some(MacroBody::List(items))) => self.eval_format(mac, items, env),
            ("vec", Some(MacroBody::List(items))) => {
                Val::List(items.iter().map(|item| self.eval(item, env)).collect())
            }
            ("vec", Some(MacroBody::Repeat(pair))) => {
                let val = self.eval(&pair[0], env);
                match self.eval(&pair[1], env) {
                    Val::Int(n) if (0..=MAX_LIST as i64).contains(&n) => {
                        Val::List(vec![val; n as usize])
                    }
                    Val::Poison => Val::Poison,
                    _ => self.invalid(mac, format!("`vec![x; n]` needs 0 ≤ n ≤ {MAX_LIST}")),
                }
            }
            _ => self.unsupported(
                mac,
                format!("the `{name}!` macro isn't supported (only `format!` and `vec!`)"),
            ),
        }
    }

    fn eval_format(&mut self, mac: &'a syn::Macro, items: &'a [Expr], env: &Env<'a>) -> Val<'a> {
        let mut parsed = items.iter();
        let Some(Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(fmt),
            ..
        })) = parsed.next()
        else {
            return self.invalid(mac, "`format!` needs a string literal first");
        };
        let mut positional = Vec::new();
        let mut named = Vec::new();
        for arg in parsed {
            match arg {
                Expr::Assign(assign) => {
                    let name = assign.left.to_token_stream().to_string();
                    named.push((name, self.eval(&assign.right, env)));
                }
                other => positional.push(self.eval(other, env)),
            }
        }
        let pieces = match format::parse(&fmt.value()) {
            Ok(pieces) => pieces,
            Err(message) => return self.invalid(fmt, message),
        };
        let mut out = String::new();
        let mut next = 0;
        for piece in pieces {
            let (arg, spec) = match piece {
                Piece::Lit(text) => {
                    out.push_str(&text);
                    continue;
                }
                Piece::Arg { arg, spec } => (arg, spec),
            };
            let val = match &arg {
                ArgRef::Next => {
                    next += 1;
                    positional.get(next - 1).cloned()
                }
                ArgRef::Index(i) => positional.get(*i).cloned(),
                ArgRef::Name(name) => named
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.clone())
                    .or_else(|| env.get(name)),
            };
            let Some(val) = val else {
                return self.invalid(
                    fmt,
                    format!("format string refers to a missing argument ({arg:?})"),
                );
            };
            let numeric = matches!(val, Val::Int(_) | Val::Float(_));
            let text = match (&val, spec.precision) {
                (Val::Poison, _) => return Val::Poison,
                (Val::Float(f), Some(p)) => format!("{f:.p$}"),
                (Val::Int(n), Some(_)) => n.to_string(),
                (Val::Str(s), _) if spec.debug => format!("{s:?}"),
                (Val::Str(s), Some(p)) => s.chars().take(p).collect(),
                (val, _) => match display(val) {
                    Some(text) => text,
                    None => {
                        let message = format!("can't format {}", val.describe());
                        return self.invalid(fmt, message);
                    }
                },
            };
            out.push_str(&format::pad(text, &spec, numeric));
        }
        Val::Str(out)
    }

    pub(super) fn eval_method_call(
        &mut self,
        call: &'a syn::ExprMethodCall,
        env: &Env<'a>,
    ) -> Val<'a> {
        let receiver = self.eval(&call.receiver, env);
        let args = self.eval_args(&call.args, env);
        let method = call.method.to_string();
        if matches!(receiver, Val::Poison) {
            return Val::Poison;
        }
        if let Val::Context(name) = &receiver {
            return self.unsupported(
                call,
                format!("`{name}.{method}(..)` needs a live gpui context; use compiled mode"),
            );
        }
        if matches!(
            method.as_str(),
            "clone"
                | "into"
                | "to_owned"
                | "into_any_element"
                | "into_element"
                | "into_any"
                | "as_ref"
                | "borrow"
        ) && args.is_empty()
        {
            return receiver;
        }
        match receiver {
            Val::Element(el) => self.element_method(el, &method, args, call),
            Val::Style(calls) => match self.styled_call(&method, args, call) {
                Some(Some(new_call)) => Val::Style(calls.into_iter().chain([new_call]).collect()),
                Some(None) => Val::Poison,
                None => self.unsupported(
                    &call.method,
                    format!("`.{method}(..)` isn't a `Styled` method"),
                ),
            },
            Val::Struct(s) => {
                let name = format!("{}::{method}", s.ty);
                match self.fns.get(&name).copied() {
                    Some(def) if def.takes_self() => {
                        self.call_fn(def, Some(Val::Struct(s)), args, call)
                    }
                    _ => self.invalid(
                        &call.method,
                        format!("no method `{method}` on `{}` in this snippet", s.ty),
                    ),
                }
            }
            receiver => self.value_method(receiver, &method, args, call),
        }
    }

    fn element_method(
        &mut self,
        mut el: Element,
        method: &str,
        args: Vec<Val<'a>>,
        call: &'a syn::ExprMethodCall,
    ) -> Val<'a> {
        let at = &call.method;
        let mut args = args.into_iter();
        let mut next = move || args.next().unwrap_or(Val::Poison);
        let argc = call.args.len();
        let expect = |this: &mut Self, n: usize| -> bool {
            if argc == n {
                true
            } else {
                this.invalid(
                    at,
                    format!("`.{method}()` takes {n} argument(s), found {argc}"),
                );
                false
            }
        };
        match method {
            "child" | "children" if el.kind != ElementKind::Div => {
                let message = format!("`{}()` can't have children; wrap it in a `div()`", el.kind.name());
                self.invalid(at, message);
                Val::Element(el)
            }
            "child" => {
                if !expect(self, 1) {
                    return Val::Element(el);
                }
                if let Some(node) = self.child_node(next(), &call.args[0]) {
                    el.children.push(node);
                }
                Val::Element(el)
            }
            "children" => {
                if !expect(self, 1) {
                    return Val::Element(el);
                }
                match next() {
                    Val::List(items) => {
                        for item in items {
                            if let Some(node) = self.child_node(item, &call.args[0]) {
                                el.children.push(node);
                            }
                        }
                    }
                    Val::Opt(item) => {
                        if let Some(node) =
                            item.and_then(|item| self.child_node(*item, &call.args[0]))
                        {
                            el.children.push(node);
                        }
                    }
                    Val::Poison => {}
                    other => {
                        let message = format!(
                            "`.children(..)` needs a list or iterator, found {}",
                            other.describe()
                        );
                        self.invalid(&call.args[0], message);
                    }
                }
                Val::Element(el)
            }
            "when" | "when_else" => {
                let n = if method == "when" { 2 } else { 3 };
                if !expect(self, n) {
                    return Val::Element(el);
                }
                let cond = next();
                let then = next();
                let otherwise = if n == 3 { Some(next()) } else { None };
                match cond {
                    Val::Bool(true) => self.call_value(then, vec![Val::Element(el)], call),
                    Val::Bool(false) => match otherwise {
                        Some(otherwise) => self.call_value(otherwise, vec![Val::Element(el)], call),
                        None => Val::Element(el),
                    },
                    Val::Poison => Val::Element(el),
                    other => {
                        let message = format!(
                            "`.{method}(..)` needs a bool condition, found {}",
                            other.describe()
                        );
                        self.invalid(&call.args[0], message);
                        Val::Element(el)
                    }
                }
            }
            "when_some" | "when_none" => {
                if !expect(self, 2) {
                    return Val::Element(el);
                }
                let (opt, then) = (next(), next());
                match (method, opt) {
                    ("when_some", Val::Opt(Some(val))) => {
                        self.call_value(then, vec![Val::Element(el), *val], call)
                    }
                    ("when_none", Val::Opt(None)) => {
                        self.call_value(then, vec![Val::Element(el)], call)
                    }
                    (_, Val::Opt(_)) => Val::Element(el),
                    (_, Val::Poison) => Val::Element(el),
                    (_, other) => {
                        let message = format!(
                            "`.{method}(..)` needs an Option, found {}",
                            other.describe()
                        );
                        self.invalid(&call.args[0], message);
                        Val::Element(el)
                    }
                }
            }
            "map" => {
                if !expect(self, 1) {
                    return Val::Element(el);
                }
                self.call_value(next(), vec![Val::Element(el)], call)
            }
            "id" | "group" => {
                if !expect(self, 1) {
                    return Val::Element(el);
                }
                let value = match next() {
                    Val::Str(s) => Value::Str(s),
                    Val::Int(n) if method == "id" && n >= 0 => Value::Number(n as f64),
                    Val::Poison => return Val::Element(el),
                    other => {
                        let message =
                            format!("`.{method}(..)` needs a string, found {}", other.describe());
                        {
                            self.invalid(&call.args[0], message);
                            return Val::Element(el);
                        }
                    }
                };
                el.calls.push(Call {
                    method: method.into(),
                    args: vec![value],
                });
                Val::Element(el)
            }
            "hover" | "active" | "group_hover" | "group_active" => {
                let grouped = method.starts_with("group_");
                if !expect(self, if grouped { 2 } else { 1 }) {
                    return Val::Element(el);
                }
                if method.ends_with("active") && !el.has_call("id") {
                    {
                        self.invalid(at, format!("`.{method}(..)` needs `.id(..)` first"));
                        return Val::Element(el);
                    }
                }
                let mut call_args = Vec::new();
                if grouped {
                    match next() {
                        Val::Str(group) => call_args.push(Value::Str(group)),
                        Val::Poison => return Val::Element(el),
                        other => {
                            let message =
                                format!("group name must be a string, found {}", other.describe());
                            {
                                self.invalid(&call.args[0], message);
                                return Val::Element(el);
                            }
                        }
                    }
                }
                match self.call_value(next(), vec![Val::Style(Vec::new())], call) {
                    Val::Style(calls) => call_args.push(Value::Style(calls)),
                    Val::Poison => return Val::Element(el),
                    other => {
                        let message = format!(
                            "the `.{method}(..)` closure must return the style, found {}",
                            other.describe()
                        );
                        {
                            self.invalid(at, message);
                            return Val::Element(el);
                        }
                    }
                }
                el.calls.push(Call {
                    method: method.into(),
                    args: call_args,
                });
                Val::Element(el)
            }
            "overflow_scroll" | "overflow_x_scroll" | "overflow_y_scroll" => {
                if !expect(self, 0) {
                    return Val::Element(el);
                }
                if !el.has_call("id") {
                    {
                        self.invalid(at, format!("`.{method}()` needs `.id(..)` first (only stateful elements scroll)"));
                        return Val::Element(el);
                    }
                }
                el.calls.push(Call {
                    method: method.into(),
                    args: Vec::new(),
                });
                Val::Element(el)
            }
            "path" if el.kind == ElementKind::Svg => {
                if !expect(self, 1) {
                    return Val::Element(el);
                }
                match next() {
                    Val::Str(path) => {
                        el.calls.push(Call {
                            method: "path".into(),
                            args: vec![Value::Str(path)],
                        });
                        Val::Element(el)
                    }
                    Val::Poison => Val::Element(el),
                    other => {
                        let message =
                            format!("`.path(..)` needs a string, found {}", other.describe());
                        self.invalid(&call.args[0], message);
                        Val::Element(el)
                    }
                }
            }
            "object_fit" | "grayscale" if matches!(el.kind, ElementKind::Img { .. }) => {
                if !expect(self, 1) {
                    return Val::Element(el);
                }
                let value = match (method, next()) {
                    ("object_fit", Val::Variant(ty, v))
                        if ty == "ObjectFit"
                            && ["Fill", "Contain", "Cover", "ScaleDown", "None"]
                                .contains(&v.as_str()) =>
                    {
                        Value::Variant(ty, v)
                    }
                    ("grayscale", Val::Bool(b)) => Value::Bool(b),
                    (_, Val::Poison) => return Val::Element(el),
                    (_, other) => {
                        let message = format!(
                            "unexpected argument to `.{method}(..)`: {}",
                            other.describe()
                        );
                        {
                            self.invalid(&call.args[0], message);
                            return Val::Element(el);
                        }
                    }
                };
                el.calls.push(Call {
                    method: method.into(),
                    args: vec![value],
                });
                Val::Element(el)
            }
            _ => match self.styled_call(method, (0..argc).map(|_| next()).collect(), call) {
                Some(Some(new_call)) => {
                    el.calls.push(new_call);
                    Val::Element(el)
                }
                // On errors, report and carry on with the element so the rest
                // of the chain is still checked.
                Some(None) => Val::Element(el),
                None if method.starts_with("on_")
                    || method.starts_with("capture_")
                    || INTERACTIVE_METHODS.contains(&method) =>
                {
                    self.unsupported(at, format!("`.{method}(..)` needs event handling or focus, which needs compiled mode"));
                    Val::Element(el)
                }
                None => {
                    let message = format!(
                        "`.{method}(..)` isn't a method the interpreter knows on `{}()`",
                        el.kind.name()
                    );
                    self.unsupported(at, message);
                    Val::Element(el)
                }
            },
        }
    }

    /// Validates a `Styled` method call. `None` if `method` isn't `Styled`,
    /// `Some(None)` if it is but the arguments were wrong (already reported).
    fn styled_call(
        &mut self,
        method: &str,
        args: Vec<Val<'a>>,
        call: &'a syn::ExprMethodCall,
    ) -> Option<Option<Call>> {
        let (_, kinds) = styled::lookup(method)?;
        if kinds.len() != args.len() {
            let mut message = format!(
                "`.{method}()` takes {} argument(s), found {}",
                kinds.len(),
                args.len()
            );
            // gpui 0.2's no-argument forms set the factor to 1.
            if args.is_empty() && matches!(method, "flex_grow" | "flex_shrink") {
                message += &format!("; gpui-pre takes the factor, so write `.{method}_1()` or `.{method}(1.)`");
            }
            self.invalid(&call.method, message);
            return Some(None);
        }
        let mut values = Vec::with_capacity(args.len());
        for (i, (kind, arg)) in kinds.iter().zip(args).enumerate() {
            match self.coerce(arg, *kind, method, &call.args[i]) {
                Some(value) => values.push(value),
                None => return Some(None),
            }
        }
        Some(Some(Call {
            method: method.into(),
            args: values,
        }))
    }

    fn coerce(&mut self, val: Val<'a>, kind: ArgKind, method: &str, at: &Expr) -> Option<Value> {
        use ArgKind as K;
        let value = match (kind, val) {
            (_, Val::Poison) => return None,
            (K::Hsla | K::Fill, Val::Color(color)) => Value::Color(color),
            (K::Length, Val::Length(len)) => Value::Length(len),
            (K::DefiniteLength, Val::Length(len)) if len != Length::Auto => Value::Length(len),
            (K::AbsoluteLength, Val::Length(len @ (Length::Px(_) | Length::Rems(_)))) => {
                Value::Length(len)
            }
            (K::SharedString, Val::Str(s)) => Value::Str(s),
            (K::F32, Val::Float(f)) => Value::Number(f),
            (K::U16, Val::Int(n)) if (0..=u16::MAX as i64).contains(&n) => Value::Number(n as f64),
            (K::I16, Val::Int(n)) if (i16::MIN as i64..=i16::MAX as i64).contains(&n) => {
                Value::Number(n as f64)
            }
            (K::Usize, Val::Int(n)) if n >= 0 => Value::Number(n as f64),
            (K::Enum(ty), Val::Variant(vty, variant))
                if vty == ty && styled::enum_variants(ty).contains(&variant.as_str()) =>
            {
                Value::Variant(vty, variant)
            }
            (K::Shadows, Val::List(items)) => {
                let mut shadows = Vec::new();
                for item in items {
                    shadows.push(self.box_shadow(item, at)?);
                }
                Value::Shadows(shadows)
            }
            (K::Unsupported(ty), _) => {
                self.unsupported(
                    at,
                    format!("`.{method}(..)` takes a `{ty}`, which the interpreter can't build"),
                );
                return None;
            }
            (kind, val) => {
                let message = format!(
                    "`.{method}(..)` expects {}, found {}",
                    kind.describe(),
                    val.describe()
                );
                self.invalid(at, message);
                return None;
            }
        };
        Some(value)
    }

    fn box_shadow(&mut self, item: Val<'a>, at: &Expr) -> Option<crate::ir::Shadow> {
        let fields = match &item {
            Val::Struct(s) if s.ty == "BoxShadow" => &s.fields,
            Val::Poison => return None,
            other => {
                let message = format!(
                    "`.shadow(..)` expects `BoxShadow` values, found {}",
                    other.describe()
                );
                self.invalid(at, message);
                return None;
            }
        };
        let field = |name: &str| {
            fields
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        };
        let px = |val: Option<Val<'a>>| match val {
            Some(Val::Length(Length::Px(v))) => Some(v),
            _ => None,
        };
        let color = match field("color") {
            Some(Val::Color(color)) => Some(color),
            _ => None,
        };
        let offset = match field("offset") {
            Some(Val::Struct(point)) if point.ty == "Point" => {
                let get = |n: &str| {
                    point
                        .fields
                        .iter()
                        .find(|(f, _)| f == n)
                        .map(|(_, v)| v.clone())
                };
                px(get("x")).zip(px(get("y")))
            }
            _ => None,
        };
        // gpui-pre added `inset`; snippets written for gpui 0.2 leave it out.
        let inset = match field("inset") {
            None => Some(false),
            Some(Val::Bool(inset)) => Some(inset),
            Some(_) => None,
        };
        match (
            color,
            offset,
            px(field("blur_radius")),
            px(field("spread_radius")),
            inset,
        ) {
            (Some(color), Some(offset), Some(blur_radius), Some(spread_radius), Some(inset)) => {
                Some(crate::ir::Shadow {
                    color,
                    offset,
                    blur_radius,
                    spread_radius,
                    inset,
                })
            }
            _ => {
                self.invalid(
                    at,
                    "`BoxShadow` needs `color` (a color), `offset: point(px(..), px(..))`, \
                     `blur_radius: px(..)`, `spread_radius: px(..)` and `inset` (a bool)",
                );
                None
            }
        }
    }

    fn value_method(
        &mut self,
        receiver: Val<'a>,
        method: &str,
        args: Vec<Val<'a>>,
        call: &'a syn::ExprMethodCall,
    ) -> Val<'a> {
        use Val::*;
        if args.iter().any(|arg| matches!(arg, Poison)) {
            return Poison;
        }
        let at = &call.method;
        match (receiver, method, args.as_slice()) {
            // Iterators are eager lists here, so adapters that only change
            // ownership or laziness are identities.
            (
                List(items),
                "iter" | "into_iter" | "iter_mut" | "cloned" | "copied" | "collect" | "to_vec"
                | "into_iter_sorted",
                [],
            ) => List(items),
            (List(items), "len" | "count", []) => Int(items.len() as i64),
            (List(items), "is_empty", []) => Bool(items.is_empty()),
            (List(items), "enumerate", []) => List(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| Tuple(vec![Int(i as i64), v]))
                    .collect(),
            ),
            (List(items), "rev", []) => List(items.into_iter().rev().collect()),
            (List(items), "take", [Int(n)]) => List(items.into_iter().take(*n as usize).collect()),
            (List(items), "skip", [Int(n)]) => List(items.into_iter().skip(*n as usize).collect()),
            (List(items), "first", []) => Opt(items.into_iter().next().map(Box::new)),
            (List(items), "last", []) => Opt(items.into_iter().last().map(Box::new)),
            (List(items), "chain", [List(more)]) => {
                List(items.into_iter().chain(more.iter().cloned()).collect())
            }
            (List(items), "zip", [List(more)]) => List(
                items
                    .into_iter()
                    .zip(more.iter().cloned())
                    .map(|(a, b)| Tuple(vec![a, b]))
                    .collect(),
            ),
            (List(items), "map", [f]) => {
                let f = f.clone();
                List(
                    items
                        .into_iter()
                        .map(|item| self.call_value(f.clone(), vec![item], call))
                        .collect(),
                )
            }
            (List(items), "filter", [f]) => {
                let f = f.clone();
                let mut kept = Vec::new();
                for item in items {
                    match self.call_value(f.clone(), vec![item.clone()], call) {
                        Bool(true) => kept.push(item),
                        Bool(false) => {}
                        Poison => return Poison,
                        other => {
                            let message = format!(
                                "`filter` closure must return a bool, found {}",
                                other.describe()
                            );
                            return self.invalid(at, message);
                        }
                    }
                }
                List(kept)
            }
            (List(items), "join", [Str(sep)]) => {
                let mut parts = Vec::new();
                for item in items {
                    match display(&item) {
                        Some(text) => parts.push(text),
                        None => return self.invalid(at, format!("can't join {}", item.describe())),
                    }
                }
                Str(parts.join(sep))
            }
            (List(items), "sum", []) => {
                let mut total = Int(0);
                for item in items {
                    total = match (total, item) {
                        (Int(a), Int(b)) => Int(a + b),
                        (Int(a), Float(b)) | (Float(b), Int(a)) => Float(a as f64 + b),
                        (Float(a), Float(b)) => Float(a + b),
                        (_, other) => {
                            return self.invalid(at, format!("can't sum {}", other.describe()));
                        }
                    };
                }
                total
            }

            (Str(s), "to_string" | "as_str" | "to_owned", []) => Str(s),
            (Str(s), "to_uppercase", []) => Str(s.to_uppercase()),
            (Str(s), "to_lowercase", []) => Str(s.to_lowercase()),
            (Str(s), "trim", []) => Str(s.trim().to_string()),
            (Str(s), "len", []) => Int(s.len() as i64),
            (Str(s), "is_empty", []) => Bool(s.is_empty()),
            (Str(s), "repeat", [Int(n)]) if (0..=MAX_LIST as i64).contains(n) => {
                Str(s.repeat(*n as usize))
            }
            (Str(s), "replace", [Str(from), Str(to)]) => Str(s.replace(from.as_str(), to)),
            (Str(s), "contains", [Str(part)]) => Bool(s.contains(part.as_str())),
            (Str(s), "starts_with", [Str(part)]) => Bool(s.starts_with(part.as_str())),
            (Str(s), "ends_with", [Str(part)]) => Bool(s.ends_with(part.as_str())),
            (Str(s), "split", [Str(sep)]) => {
                List(s.split(sep.as_str()).map(|p| Str(p.to_string())).collect())
            }
            (Str(s), "lines", []) => List(s.lines().map(|p| Str(p.to_string())).collect()),

            (Color(color), "opacity" | "alpha", [Float(f)]) => {
                if color.is_rgba() {
                    return self.invalid(
                        at,
                        format!("`rgb(..)`/`rgba(..)` return `Rgba`, which has no `.{method}()`; use `Hsla::from(rgb(..)).{method}(..)`"),
                    );
                }
                let base = Box::new(color);
                Color(if method == "opacity" {
                    crate::ir::Color::Opacity {
                        base,
                        factor: *f as f32,
                    }
                } else {
                    crate::ir::Color::Alpha {
                        base,
                        alpha: *f as f32,
                    }
                })
            }

            (val @ (Int(_) | Float(_) | Bool(_)), "to_string", []) => Str(display(&val).unwrap()),
            (Int(n), "abs", []) => Int(n.abs()),
            (Int(n), "pow", [Int(e)]) if (0..64).contains(e) => match n.checked_pow(*e as u32) {
                Some(v) => Int(v),
                None => self.invalid(at, "overflow in `pow`"),
            },
            (Int(a), "min", [Int(b)]) => Int(a.min(*b)),
            (Int(a), "div_ceil", [Int(b)]) if *b > 0 => Int((a + b - 1).div_euclid(*b)),
            (Int(a), "rem_euclid", [Int(b)]) if *b != 0 => Int(a.rem_euclid(*b)),
            (Int(a), "saturating_sub", [Int(b)]) => Int(a.saturating_sub(*b)),
            (Int(a), "max", [Int(b)]) => Int(a.max(*b)),
            (Int(n), "clamp", [Int(lo), Int(hi)]) if lo <= hi => Int(n.clamp(*lo, *hi)),
            (Float(f), "abs", []) => Float(f.abs()),
            (Float(f), "round", []) => Float(f.round()),
            (Float(f), "floor", []) => Float(f.floor()),
            (Float(f), "ceil", []) => Float(f.ceil()),
            (Float(f), "sqrt", []) => Float(f.sqrt()),
            (Float(f), "sin", []) => Float(f.sin()),
            (Float(f), "cos", []) => Float(f.cos()),
            (Float(f), "powi", [Int(e)]) => Float(f.powi(*e as i32)),
            (Float(f), "powf", [Float(e)]) => Float(f.powf(*e)),
            (Float(a), "min", [Float(b)]) => Float(a.min(*b)),
            (Float(a), "max", [Float(b)]) => Float(a.max(*b)),
            (Float(f), "clamp", [Float(lo), Float(hi)]) if lo <= hi => Float(f.clamp(*lo, *hi)),

            (Bool(b), "then", [f]) => {
                if b {
                    let val = self.call_value(f.clone(), Vec::new(), call);
                    Opt(Some(Box::new(val)))
                } else {
                    Opt(None)
                }
            }
            (Bool(b), "then_some", [val]) => Opt(b.then(|| Box::new(val.clone()))),

            (Opt(opt), "is_some", []) => Bool(opt.is_some()),
            (Opt(opt), "is_none", []) => Bool(opt.is_none()),
            (Opt(opt), "unwrap_or", [default]) => opt.map_or_else(|| default.clone(), |v| *v),
            (Opt(Some(val)), "unwrap", []) => *val,
            (Opt(None), "unwrap", []) => self.invalid(at, "called `unwrap()` on `None`"),
            (Opt(opt), "map", [f]) => match opt {
                Some(val) => {
                    let mapped = self.call_value(f.clone(), vec![*val], call);
                    Opt(Some(Box::new(mapped)))
                }
                None => Opt(None),
            },

            (receiver, _, _) => {
                let message = format!(
                    "no method `{method}` with these arguments on {}",
                    receiver.describe()
                );
                self.unsupported(at, message)
            }
        }
    }
}

/// `Display` for values that have one.
pub(super) fn display(val: &Val) -> Option<String> {
    Some(match val {
        Val::Str(s) => s.clone(),
        Val::Int(n) => n.to_string(),
        Val::Float(f) => f.to_string(),
        Val::Bool(b) => b.to_string(),
        _ => return None,
    })
}
