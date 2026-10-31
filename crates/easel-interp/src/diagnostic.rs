use proc_macro2::Span;
use quote::ToTokens;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    /// Valid GPUI that the interpreter can't evaluate. Compiling may work.
    Unsupported,
    /// Code that would not compile either, e.g. a wrong argument type.
    Invalid,
    /// The source isn't parseable Rust.
    Syntax,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    pub message: String,
    /// 1-based line and column in the submitted source.
    pub line: usize,
    pub column: usize,
    /// The offending code, abbreviated.
    pub snippet: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            DiagnosticKind::Unsupported => "unsupported",
            DiagnosticKind::Invalid => "invalid",
            DiagnosticKind::Syntax => "syntax error",
        };
        write!(f, "{}:{}: {kind}: {}", self.line, self.column, self.message)?;
        if !self.snippet.is_empty() {
            write!(f, " (`{}`)", self.snippet)?;
        }
        Ok(())
    }
}

/// Maps spans back to the user's source, undoing the `{` we prepend when the
/// source is parsed as a bare block.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SourceMap {
    pub first_line_shift: usize,
}

impl SourceMap {
    pub fn locate(&self, span: Span) -> (usize, usize) {
        let start = span.start();
        let mut column = start.column + 1;
        if start.line == 1 {
            column = column.saturating_sub(self.first_line_shift).max(1);
        }
        (start.line.max(1), column)
    }

    /// A diagnostic located at the first token of `node`.
    pub fn diagnostic(
        &self,
        kind: DiagnosticKind,
        node: &dyn ToTokens,
        message: impl Into<String>,
    ) -> Diagnostic {
        let tokens = node.to_token_stream();
        let span = tokens
            .clone()
            .into_iter()
            .next()
            .map_or_else(Span::call_site, |t| t.span());
        self.diagnostic_at(kind, span, &tokens.to_string(), message)
    }

    pub fn diagnostic_at(
        &self,
        kind: DiagnosticKind,
        span: Span,
        snippet: &str,
        message: impl Into<String>,
    ) -> Diagnostic {
        let (line, column) = self.locate(span);
        Diagnostic {
            kind,
            message: message.into(),
            line,
            column,
            snippet: abbreviate(snippet),
        }
    }
}

fn abbreviate(code: &str) -> String {
    const MAX: usize = 60;
    let code = code.split_whitespace().collect::<Vec<_>>().join(" ");
    if code.chars().count() <= MAX {
        code
    } else {
        format!("{}…", code.chars().take(MAX).collect::<String>())
    }
}
