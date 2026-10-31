//! Converts interpreted [`Value`]s into gpui argument types.

use crate::ir::{Color, Length, Shadow, Value};
use gpui::{AbsoluteLength, BoxShadow, DefiniteLength, Fill, Hsla, SharedString, point, px, rems, relative};

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("style method #{0} has no renderer")]
    UnsupportedMethod(usize),
    #[error("`{1}` isn't a `{0}`")]
    UnknownVariant(&'static str, String),
    #[error("expected {expected}, found {found:?}")]
    WrongArg { expected: &'static str, found: Value },
}

fn wrong(expected: &'static str, found: &Value) -> RenderError {
    RenderError::WrongArg {
        expected,
        found: found.clone(),
    }
}

pub(crate) fn color_to_hsla(color: &Color) -> Hsla {
    match color {
        Color::Rgb(hex) => gpui::rgb(*hex).into(),
        Color::Rgba(hex) => gpui::rgba(*hex).into(),
        Color::Hsla { h, s, l, a } => gpui::hsla(*h, *s, *l, *a),
        Color::Named(name) => match name.as_str() {
            "red" => gpui::red(),
            "green" => gpui::green(),
            "blue" => gpui::blue(),
            "yellow" => gpui::yellow(),
            "black" => gpui::black(),
            "white" => gpui::white(),
            "transparent_white" => gpui::transparent_white(),
            _ => gpui::transparent_black(),
        },
        Color::Opacity { base, factor } => color_to_hsla(base).opacity(*factor),
        Color::Alpha { base, alpha } => color_to_hsla(base).alpha(*alpha),
    }
}

pub(crate) fn hsla(value: &Value) -> Result<Hsla, RenderError> {
    match value {
        Value::Color(color) => Ok(color_to_hsla(color)),
        other => Err(wrong("a color", other)),
    }
}

pub(crate) fn fill(value: &Value) -> Result<Fill, RenderError> {
    hsla(value).map(Fill::from)
}

pub(crate) fn length(value: &Value) -> Result<gpui::Length, RenderError> {
    match value {
        Value::Length(Length::Auto) => Ok(gpui::Length::Auto),
        other => definite_length(other).map(gpui::Length::Definite),
    }
}

pub(crate) fn definite_length(value: &Value) -> Result<DefiniteLength, RenderError> {
    match value {
        Value::Length(Length::Relative(f)) => Ok(relative(*f)),
        other => absolute_length(other).map(DefiniteLength::Absolute),
    }
}

pub(crate) fn absolute_length(value: &Value) -> Result<AbsoluteLength, RenderError> {
    match value {
        Value::Length(Length::Px(v)) => Ok(px(*v).into()),
        Value::Length(Length::Rems(v)) => Ok(rems(*v).into()),
        other => Err(wrong("a px or rems length", other)),
    }
}

pub(crate) fn shared_string(value: &Value) -> Result<SharedString, RenderError> {
    match value {
        Value::Str(s) => Ok(s.clone().into()),
        other => Err(wrong("a string", other)),
    }
}

fn number(value: &Value) -> Result<f64, RenderError> {
    match value {
        Value::Number(n) => Ok(*n),
        other => Err(wrong("a number", other)),
    }
}

pub(crate) fn num_f32(value: &Value) -> Result<f32, RenderError> {
    number(value).map(|n| n as f32)
}

pub(crate) fn num_u16(value: &Value) -> Result<u16, RenderError> {
    number(value).map(|n| n as u16)
}

pub(crate) fn num_i16(value: &Value) -> Result<i16, RenderError> {
    number(value).map(|n| n as i16)
}

pub(crate) fn num_usize(value: &Value) -> Result<usize, RenderError> {
    number(value).map(|n| n as usize)
}

pub(crate) fn shadows(value: &Value) -> Result<Vec<BoxShadow>, RenderError> {
    match value {
        Value::Shadows(shadows) => Ok(shadows.iter().map(box_shadow).collect()),
        other => Err(wrong("box shadows", other)),
    }
}

fn box_shadow(shadow: &Shadow) -> BoxShadow {
    BoxShadow {
        color: color_to_hsla(&shadow.color),
        offset: point(px(shadow.offset.0), px(shadow.offset.1)),
        blur_radius: px(shadow.blur_radius),
        spread_radius: px(shadow.spread_radius),
        inset: shadow.inset,
    }
}

/// The variant name of a `Value::Variant` of type `ty`.
pub(crate) fn variant<'v>(value: &'v Value, ty: &'static str) -> Result<&'v str, RenderError> {
    match value {
        Value::Variant(vty, variant) if vty == ty => Ok(variant),
        other => Err(wrong(ty, other)),
    }
}
