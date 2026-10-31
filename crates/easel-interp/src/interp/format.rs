//! A `format!` subset: positional, indexed and named arguments, `{:?}`,
//! fill/alignment, width, zero padding and precision.

#[derive(Debug, PartialEq)]
pub(super) enum Piece {
    Lit(String),
    Arg { arg: ArgRef, spec: Spec },
}

#[derive(Debug, PartialEq)]
pub(super) enum ArgRef {
    Next,
    Index(usize),
    Name(String),
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Spec {
    fill: Option<char>,
    align: Option<char>,
    zero: bool,
    width: Option<usize>,
    pub precision: Option<usize>,
    pub debug: bool,
}

pub(super) fn parse(fmt: &str) -> Result<Vec<Piece>, String> {
    let mut pieces = Vec::new();
    let mut lit = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                lit.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                lit.push('}');
            }
            '}' => return Err("unmatched `}` in format string".into()),
            '{' => {
                let mut inner = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => inner.push(c),
                        None => return Err("unclosed `{` in format string".into()),
                    }
                }
                if !lit.is_empty() {
                    pieces.push(Piece::Lit(std::mem::take(&mut lit)));
                }
                let (name, spec) = inner.split_once(':').unwrap_or((&inner, ""));
                let name = name.trim();
                let arg = if name.is_empty() {
                    ArgRef::Next
                } else if let Ok(index) = name.parse() {
                    ArgRef::Index(index)
                } else {
                    ArgRef::Name(name.to_string())
                };
                pieces.push(Piece::Arg {
                    arg,
                    spec: parse_spec(spec)?,
                });
            }
            c => lit.push(c),
        }
    }
    if !lit.is_empty() {
        pieces.push(Piece::Lit(lit));
    }
    Ok(pieces)
}

fn parse_spec(spec: &str) -> Result<Spec, String> {
    let mut out = Spec::default();
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let is_align = |c: char| matches!(c, '<' | '^' | '>');
    if chars.len() >= 2 && is_align(chars[1]) {
        out.fill = Some(chars[0]);
        out.align = Some(chars[1]);
        i = 2;
    } else if chars.first().copied().is_some_and(is_align) {
        out.align = Some(chars[0]);
        i = 1;
    }
    if chars.get(i) == Some(&'0') {
        out.zero = true;
        i += 1;
    }
    let digits = |i: &mut usize| {
        let start = *i;
        while chars.get(*i).is_some_and(char::is_ascii_digit) {
            *i += 1;
        }
        (start < *i).then(|| chars[start..*i].iter().collect::<String>().parse().unwrap())
    };
    out.width = digits(&mut i);
    if chars.get(i) == Some(&'.') {
        i += 1;
        out.precision = Some(digits(&mut i).ok_or("expected a precision after `.`")?);
    }
    if chars.get(i) == Some(&'?') {
        out.debug = true;
        i += 1;
    }
    if i != chars.len() {
        return Err(format!("unsupported format spec `:{spec}`"));
    }
    Ok(out)
}

/// Pads an already-formatted value to the spec's width.
pub(super) fn pad(text: String, spec: &Spec, numeric: bool) -> String {
    let Some(width) = spec.width else {
        return text;
    };
    let len = text.chars().count();
    if len >= width {
        return text;
    }
    let missing = width - len;
    if spec.zero && numeric {
        let (sign, digits) = match text.strip_prefix('-') {
            Some(rest) => ("-", rest.to_string()),
            None => ("", text),
        };
        return format!("{sign}{}{digits}", "0".repeat(missing));
    }
    let fill = spec.fill.unwrap_or(' ').to_string();
    let align = spec.align.unwrap_or(if numeric { '>' } else { '<' });
    let (left, right) = match align {
        '>' => (missing, 0),
        '^' => (missing / 2, missing - missing / 2),
        _ => (0, missing),
    };
    format!("{}{text}{}", fill.repeat(left), fill.repeat(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pieces() {
        let pieces = parse("a {} {1} {name:>4} {{x}}").unwrap();
        assert_eq!(pieces.len(), 7);
        assert_eq!(pieces[6], Piece::Lit(" {x}".into()));
    }

    #[test]
    fn pads() {
        let spec = parse_spec("05").unwrap();
        assert_eq!(pad("-7".into(), &spec, true), "-0007");
        let spec = parse_spec("*^6").unwrap();
        assert_eq!(pad("ab".into(), &spec, false), "**ab**");
        assert!(parse_spec("x").is_err());
    }
}
