//! `format!` and `vec!` bodies are opaque tokens to `syn`. They're parsed up
//! front, including macros nested inside them, so the evaluator can borrow
//! the parsed expressions for as long as it borrows the rest of the source.

use std::collections::HashMap;
use syn::{Expr, Macro, punctuated::Punctuated, visit::Visit};

pub(super) enum MacroBody {
    List(Box<[Expr]>),
    /// `vec![item; count]`
    Repeat(Box<[Expr; 2]>),
    Error(syn::Error),
}

#[derive(Default)]
pub(super) struct Macros {
    /// Keyed by the address of the `syn::Macro`. Every macro lives either in
    /// the source tree or inside one of these boxed bodies, and neither moves
    /// while evaluation runs.
    bodies: HashMap<usize, MacroBody>,
}

impl Macros {
    pub fn collect_file(file: &syn::File) -> Self {
        let mut macros = Self::default();
        macros.visit_file(file);
        macros
    }

    pub fn collect_block(block: &syn::Block) -> Self {
        let mut macros = Self::default();
        macros.visit_block(block);
        macros
    }

    pub fn get(&self, mac: &Macro) -> Option<&MacroBody> {
        self.bodies.get(&(mac as *const Macro as usize))
    }
}

impl<'ast> Visit<'ast> for Macros {
    fn visit_macro(&mut self, mac: &'ast Macro) {
        let name = mac.path.segments.last().map(|s| s.ident.to_string());
        let body = match name.as_deref() {
            Some("format") => {
                match mac.parse_body_with(Punctuated::<Expr, syn::Token![,]>::parse_terminated) {
                    Ok(items) => MacroBody::List(items.into_iter().collect()),
                    Err(err) => MacroBody::Error(err),
                }
            }
            Some("vec") => {
                match mac.parse_body_with(Punctuated::<Expr, syn::Token![,]>::parse_terminated) {
                    Ok(items) => MacroBody::List(items.into_iter().collect()),
                    Err(_) => match mac.parse_body_with(parse_repeat) {
                        Ok(pair) => MacroBody::Repeat(Box::new(pair)),
                        Err(err) => MacroBody::Error(err),
                    },
                }
            }
            _ => return,
        };
        // Visit the parsed body in its final (boxed) location so nested
        // macro addresses stay valid.
        match &body {
            MacroBody::List(items) => items.iter().for_each(|item| self.visit_expr(item)),
            MacroBody::Repeat(pair) => pair.iter().for_each(|item| self.visit_expr(item)),
            MacroBody::Error(_) => {}
        }
        self.bodies.insert(mac as *const Macro as usize, body);
    }
}

fn parse_repeat(input: syn::parse::ParseStream) -> syn::Result<[Expr; 2]> {
    let item: Expr = input.parse()?;
    input.parse::<syn::Token![;]>()?;
    let count: Expr = input.parse()?;
    Ok([item, count])
}
