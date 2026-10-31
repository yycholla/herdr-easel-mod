//! The `Styled` method table, generated from gpui's rustdoc by `tools/gen-styled`.

#[path = "generated/styled_methods.rs"]
mod table;

pub(crate) use table::{ENUMS, STYLED_METHODS};

/// What a `Styled` method parameter accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgKind {
    /// `px`, `rems`, `relative` or `auto()`.
    Length,
    /// `px`, `rems` or `relative`.
    DefiniteLength,
    /// `px` or `rems`.
    AbsoluteLength,
    Hsla,
    Fill,
    SharedString,
    F32,
    U16,
    I16,
    Usize,
    /// `Vec<BoxShadow>`
    Shadows,
    /// A variant or associated constant of the named type.
    Enum(&'static str),
    /// A parameter type the interpreter can't build, e.g. `Font`.
    Unsupported(&'static str),
}

impl ArgKind {
    pub fn describe(self) -> String {
        match self {
            ArgKind::Length => "a length (`px(..)`, `rems(..)`, `relative(..)` or `auto()`)".into(),
            ArgKind::DefiniteLength => "a length (`px(..)`, `rems(..)` or `relative(..)`)".into(),
            ArgKind::AbsoluteLength => "an absolute length (`px(..)` or `rems(..)`)".into(),
            ArgKind::Hsla => {
                "a color (`rgb(..)`, `rgba(..)`, `hsla(..)` or e.g. `gpui::red()`)".into()
            }
            ArgKind::Fill => {
                "a color (`rgb(..)`, `rgba(..)`, `hsla(..)` or e.g. `gpui::red()`)".into()
            }
            ArgKind::SharedString => "a string".into(),
            ArgKind::F32 => "an f32".into(),
            ArgKind::U16 | ArgKind::Usize => "an unsigned integer".into(),
            ArgKind::I16 => "an integer".into(),
            ArgKind::Shadows => {
                "a `vec![BoxShadow { color, offset, blur_radius, spread_radius, inset }]`".into()
            }
            ArgKind::Enum(name) => format!(
                "a `{name}` value, e.g. `{name}::{}`",
                enum_variants(name)[0]
            ),
            ArgKind::Unsupported(ty) => format!("a `{ty}`"),
        }
    }
}

/// The index into `STYLED_METHODS` and argument kinds of a `Styled` method.
pub fn lookup(method: &str) -> Option<(usize, &'static [ArgKind])> {
    let index = STYLED_METHODS
        .binary_search_by(|(name, _)| (*name).cmp(method))
        .ok()?;
    Some((index, STYLED_METHODS[index].1))
}

pub fn enum_variants(name: &str) -> &'static [&'static str] {
    ENUMS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(&[], |(_, variants)| variants)
}

pub fn is_enum(name: &str) -> bool {
    ENUMS.iter().any(|(n, _)| *n == name)
}

/// All `Styled` method names, for docs and coverage checks.
pub fn method_names() -> impl Iterator<Item = &'static str> {
    STYLED_METHODS.iter().map(|(name, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_for_binary_search() {
        assert!(STYLED_METHODS.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn looks_up_common_methods() {
        assert_eq!(lookup("flex").unwrap().1, &[]);
        assert_eq!(lookup("bg").unwrap().1, &[ArgKind::Fill]);
        assert_eq!(lookup("w").unwrap().1, &[ArgKind::Length]);
        assert_eq!(
            lookup("font_weight").unwrap().1,
            &[ArgKind::Enum("FontWeight")]
        );
        assert!(lookup("on_click").is_none());
    }
}
