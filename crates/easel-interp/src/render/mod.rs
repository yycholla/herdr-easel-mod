//! Turns the interpreted tree into real gpui elements.

mod args;
#[path = "../generated/styled_apply.rs"]
mod apply;
mod rem;

pub use args::RenderError;
pub use rem::{WithRemSize, with_rem_size};

use crate::ir::{Call, Element, ElementKind, Length, Node, NodePath, Value};
use crate::styled;
use gpui::{
    AnyElement, Bounds, ElementId, InteractiveElement, IntoElement, ObjectFit, ParentElement,
    Pixels, Refineable, SharedString, StatefulInteractiveElement, StyleRefinement, Styled,
    StyledImage, div, img, svg,
};
use std::{borrow::Cow, cell::RefCell, collections::HashMap, path::Path, rc::Rc};

/// Window-space bounds of rendered nodes, keyed by path from the root. The
/// root itself is recorded by whoever lays it out (see [`record_children`]).
pub type BoundsMap = Rc<RefCell<HashMap<NodePath, Bounds<Pixels>>>>;

#[derive(Clone)]
pub struct RenderOptions {
    /// Multiplies pixel lengths, for zooming. Pair it with [`with_rem_size`]
    /// so rem-based styles scale too.
    pub scale: f32,
    /// Collects every node's bounds after prepaint, without changing layout.
    pub bounds: Option<BoundsMap>,
    /// Where relative `img(..)` and `svg().path(..)` paths resolve. Set, they
    /// are read from the filesystem directly, so they don't depend on the
    /// app's `AssetSource`; unset, they go through it as embedded assets.
    pub base_dir: Option<Rc<Path>>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { scale: 1., bounds: None, base_dir: None }
    }
}

/// Builds the gpui element for `node`. Calls that fail to convert are
/// skipped and returned as errors; the interpreter validates arguments, so
/// that only happens if the IR was built some other way.
pub fn to_element(node: &Node) -> (AnyElement, Vec<RenderError>) {
    to_element_with(node, &RenderOptions::default())
}

pub fn to_element_with(node: &Node, options: &RenderOptions) -> (AnyElement, Vec<RenderError>) {
    let mut errors = Vec::new();
    let element = build(node, Vec::new(), options, &mut errors);
    (element, errors)
}

/// A `Div::on_children_prepainted` listener recording children's bounds as
/// `parent/0`, `parent/1`, ….
pub fn record_children(map: BoundsMap, parent: NodePath) -> impl Fn(Vec<Bounds<Pixels>>, &mut gpui::Window, &mut gpui::App) + 'static {
    move |bounds, _, _| {
        let mut map = map.borrow_mut();
        for (i, b) in bounds.into_iter().enumerate() {
            let mut path = parent.clone();
            path.push(i);
            map.insert(path, b);
        }
    }
}

fn build(node: &Node, path: NodePath, options: &RenderOptions, errors: &mut Vec<RenderError>) -> AnyElement {
    match node {
        Node::Text { text } => SharedString::from(text.clone()).into_any_element(),
        Node::Element(el) => build_element(el, path, options, errors),
    }
}

/// Pixel arguments multiplied by `scale`.
fn scaled(calls: &[Call], scale: f32) -> Cow<'_, [Call]> {
    if scale == 1. {
        return Cow::Borrowed(calls);
    }
    fn value(v: &Value, scale: f32) -> Value {
        match v {
            Value::Length(Length::Px(px)) => Value::Length(Length::Px(px * scale)),
            Value::Style(calls) => Value::Style(calls.iter().map(|c| call(c, scale)).collect()),
            Value::Shadows(shadows) => Value::Shadows(
                shadows
                    .iter()
                    .map(|s| crate::ir::Shadow {
                        color: s.color.clone(),
                        offset: (s.offset.0 * scale, s.offset.1 * scale),
                        blur_radius: s.blur_radius * scale,
                        spread_radius: s.spread_radius * scale,
                        inset: s.inset,
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    fn call(c: &Call, scale: f32) -> Call {
        Call { method: c.method.clone(), args: c.args.iter().map(|v| value(v, scale)).collect() }
    }
    Cow::Owned(calls.iter().map(|c| call(c, scale)).collect())
}

/// Builds a `StyleRefinement` from `Styled` calls.
pub fn refinement(calls: &[Call], errors: &mut Vec<RenderError>) -> StyleRefinement {
    let mut style = StyleRefinement::default();
    for call in calls {
        let Some((index, _)) = styled::lookup(&call.method) else {
            continue;
        };
        match apply::apply(style.clone(), index, &call.args) {
            Ok(refined) => style = refined,
            Err(err) => errors.push(err),
        }
    }
    style
}

/// The non-`Styled` calls on an element.
#[derive(Default)]
struct Behavior {
    id: Option<ElementId>,
    group: Option<SharedString>,
    hover: Option<StyleRefinement>,
    active: Option<StyleRefinement>,
    group_hover: Option<(SharedString, StyleRefinement)>,
    group_active: Option<(SharedString, StyleRefinement)>,
    scroll: (bool, bool),
    svg_path: Option<SharedString>,
    object_fit: Option<ObjectFit>,
    grayscale: Option<bool>,
}

fn behavior(calls: &[Call], errors: &mut Vec<RenderError>) -> Behavior {
    let mut b = Behavior::default();
    let string = |args: &[Value], i: usize| match args.get(i) {
        Some(Value::Str(s)) => Some(SharedString::from(s.clone())),
        _ => None,
    };
    let mut style_arg = |args: &[Value], i: usize| match args.get(i) {
        Some(Value::Style(calls)) => refinement(calls, errors),
        _ => StyleRefinement::default(),
    };
    for call in calls {
        let args = call.args.as_slice();
        match call.method.as_str() {
            "id" => {
                b.id = match args.first() {
                    Some(Value::Str(s)) => Some(ElementId::Name(s.clone().into())),
                    Some(Value::Number(n)) => Some(ElementId::Integer(*n as u64)),
                    _ => None,
                }
            }
            "group" => b.group = string(args, 0),
            "hover" => b.hover = Some(style_arg(args, 0)),
            "active" => b.active = Some(style_arg(args, 0)),
            "group_hover" => b.group_hover = string(args, 0).map(|g| (g, style_arg(args, 1))),
            "group_active" => b.group_active = string(args, 0).map(|g| (g, style_arg(args, 1))),
            "overflow_scroll" => b.scroll = (true, true),
            "overflow_x_scroll" => b.scroll.0 = true,
            "overflow_y_scroll" => b.scroll.1 = true,
            "path" => b.svg_path = string(args, 0),
            "grayscale" => b.grayscale = matches!(args.first(), Some(Value::Bool(true))).into(),
            "object_fit" => {
                b.object_fit = match args.first() {
                    Some(Value::Variant(_, v)) => Some(match v.as_str() {
                        "Fill" => ObjectFit::Fill,
                        "Cover" => ObjectFit::Cover,
                        "ScaleDown" => ObjectFit::ScaleDown,
                        "None" => ObjectFit::None,
                        _ => ObjectFit::Contain,
                    }),
                    _ => None,
                }
            }
            _ => {}
        }
    }
    b
}

/// Applies style, interactivity and (if there's an id) stateful behavior,
/// then erases the type. A macro because `Div`, `Img` and `Svg` share these
/// traits but no common supertrait.
macro_rules! finish {
    ($el:expr, $style:expr, $b:expr, |$stateful:ident| $stateful_body:expr) => {{
        let mut el = $el;
        el.style().refine(&$style);
        let b = $b;
        if let Some(group) = b.group {
            el = el.group(group);
        }
        if let Some(hover) = b.hover {
            el = el.hover(move |s| s.refined(hover));
        }
        if let Some((group, style)) = b.group_hover {
            el = el.group_hover(group, move |s| s.refined(style));
        }
        match b.id {
            None => el.into_any_element(),
            Some(id) => {
                let mut $stateful = el.id(id);
                if let Some(active) = b.active {
                    $stateful = $stateful.active(move |s| s.refined(active));
                }
                if let Some((group, style)) = b.group_active {
                    $stateful = $stateful.group_active(group, move |s| s.refined(style));
                }
                $stateful_body.into_any_element()
            }
        }
    }};
}

fn build_element(el: &Element, path: NodePath, options: &RenderOptions, errors: &mut Vec<RenderError>) -> AnyElement {
    let calls = scaled(&el.calls, options.scale);
    let style = refinement(&calls, errors);
    let mut b = behavior(&calls, errors);
    match &el.kind {
        ElementKind::Div => {
            let children: Vec<AnyElement> = el
                .children
                .iter()
                .enumerate()
                .map(|(i, child)| {
                    let mut child_path = path.clone();
                    child_path.push(i);
                    build(child, child_path, options, errors)
                })
                .collect();
            let mut container = div().children(children);
            if let Some(map) = &options.bounds {
                container = container.on_children_prepainted(record_children(map.clone(), path));
            }
            let scroll = b.scroll;
            finish!(container, style, b, |stateful| {
                match scroll {
                    (true, true) => stateful.overflow_scroll(),
                    (true, false) => stateful.overflow_x_scroll(),
                    (false, true) => stateful.overflow_y_scroll(),
                    (false, false) => stateful,
                }
            })
        }
        ElementKind::Img { source } => {
            let mut image = match &options.base_dir {
                Some(dir) if !source.contains("://") => img(dir.join(source)),
                _ => img(source.clone()),
            };
            if let Some(fit) = b.object_fit.take() {
                image = image.object_fit(fit);
            }
            if let Some(grayscale) = b.grayscale.take() {
                image = image.grayscale(grayscale);
            }
            finish!(image, style, b, |stateful| stateful)
        }
        ElementKind::Svg => {
            let mut icon = svg();
            if let Some(path) = b.svg_path.clone() {
                icon = match &options.base_dir {
                    Some(dir) => icon.external_path(dir.join(path.as_ref()).to_string_lossy().into_owned()),
                    None => icon.path(path),
                };
            }
            finish!(icon, style, b, |stateful| stateful)
        }
    }
}
