//! The interpreted element tree. It holds no gpui types, so it can be built,
//! inspected and serialized without linking gpui.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "snake_case")]
pub enum Node {
    Element(Element),
    Text { text: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Element {
    pub kind: ElementKind,
    /// Where the element's constructor (`div()`, …) is in the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceRef>,
    /// Styling and attribute calls, in source order.
    pub calls: Vec<Call>,
    pub children: Vec<Node>,
}

impl Element {
    pub fn new(kind: ElementKind) -> Self {
        Self {
            kind,
            source: None,
            calls: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn has_call(&self, method: &str) -> bool {
        self.calls.iter().any(|call| call.method == method)
    }
}

/// A 1-based location in the snippet. Elements built in a loop or a helper
/// called several times share one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub line: usize,
    pub column: usize,
}

/// Child indices from the root to a node.
pub type NodePath = Vec<usize>;

impl Node {
    pub fn at(&self, path: &[usize]) -> Option<&Node> {
        match (path.split_first(), self) {
            (None, node) => Some(node),
            (Some((first, rest)), Node::Element(el)) => el.children.get(*first)?.at(rest),
            (Some(_), Node::Text { .. }) => None,
        }
    }

    /// All text under this node, space-separated.
    pub fn text_content(&self) -> String {
        fn collect(node: &Node, out: &mut Vec<String>) {
            match node {
                Node::Text { text } => out.push(text.clone()),
                Node::Element(el) => el.children.iter().for_each(|c| collect(c, out)),
            }
        }
        let mut parts = Vec::new();
        collect(self, &mut parts);
        parts.join(" ")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ElementKind {
    Div,
    Img { source: String },
    Svg,
}

impl ElementKind {
    pub fn name(&self) -> &'static str {
        match self {
            ElementKind::Div => "div",
            ElementKind::Img { .. } => "img",
            ElementKind::Svg => "svg",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub method: String,
    pub args: Vec<Value>,
}

/// A fully evaluated argument to a style or attribute call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Color(Color),
    Length(Length),
    Number(f64),
    Str(String),
    Bool(bool),
    /// An enum variant or associated constant, e.g. `FontWeight::BOLD`,
    /// stored as `(type, variant)`.
    Variant(String, String),
    /// The refinement built by a closure such as `.hover(|s| s.bg(..))`.
    Style(Vec<Call>),
    Shadows(Vec<Shadow>),
}

/// A `gpui::BoxShadow`, in pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shadow {
    pub color: Color,
    pub offset: (f32, f32),
    pub blur_radius: f32,
    pub spread_radius: f32,
    #[serde(default)]
    pub inset: bool,
}

/// Colors keep the constructor they were written with so rendering is exact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    Rgb(u32),
    Rgba(u32),
    Hsla {
        h: f32,
        s: f32,
        l: f32,
        a: f32,
    },
    /// A gpui color function such as `gpui::red()`.
    Named(String),
    /// `Hsla::opacity`: multiplies the alpha. `Hsla::from(c)` is factor 1.
    Opacity {
        base: Box<Color>,
        factor: f32,
    },
    /// `Hsla::alpha`: replaces the alpha.
    Alpha {
        base: Box<Color>,
        alpha: f32,
    },
}

impl Color {
    /// `rgb(..)` and `rgba(..)` build an `Rgba`, which lacks `Hsla`'s methods.
    pub fn is_rgba(&self) -> bool {
        matches!(self, Color::Rgb(_) | Color::Rgba(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Length {
    Px(f32),
    Rems(f32),
    /// A fraction of the parent, from `relative(0.5)`.
    Relative(f32),
    Auto,
}
