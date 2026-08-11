//! Shared attribute parsing.

use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, ExprLit, Lit, Meta, Result, Type};

/// Collects `///` lines into a summary (first paragraph) and description.
///
/// This is how a handler gets documented prose without repeating it in an
/// attribute — the doc comment is already the right place for it.
pub struct Docs {
    pub summary: Option<String>,
    pub description: Option<String>,
}

pub fn doc_comment(attrs: &[Attribute]) -> Docs {
    let mut lines: Vec<String> = Vec::new();

    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let Meta::NameValue(nv) = &attr.meta {
            if let Expr::Lit(ExprLit {
                lit: Lit::Str(text),
                ..
            }) = &nv.value
            {
                // Doc lines carry a leading space from `/// `.
                lines.push(
                    text.value()
                        .strip_prefix(' ')
                        .unwrap_or(&text.value())
                        .to_owned(),
                );
            }
        }
    }

    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return Docs {
            summary: None,
            description: None,
        };
    }

    // The first blank line ends the summary, matching the convention every
    // Rust doc comment already follows.
    let split = lines.iter().position(|l| l.trim().is_empty());
    match split {
        Some(index) => {
            let summary = lines[..index].join(" ").trim().to_owned();
            let description = lines[index + 1..].join("\n").trim().to_owned();
            Docs {
                summary: (!summary.is_empty()).then_some(summary),
                description: (!description.is_empty()).then_some(description),
            }
        }
        None => {
            let summary = lines.join(" ").trim().to_owned();
            Docs {
                summary: (!summary.is_empty()).then_some(summary),
                description: None,
            }
        }
    }
}

/// A `#[serde(...)]` rename convention.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenameRule {
    #[default]
    None,
    Lower,
    Upper,
    Pascal,
    Camel,
    Snake,
    ScreamingSnake,
    Kebab,
    ScreamingKebab,
}

impl RenameRule {
    pub fn parse(value: &str, span: Span) -> Result<Self> {
        Ok(match value {
            "lowercase" => RenameRule::Lower,
            "UPPERCASE" => RenameRule::Upper,
            "PascalCase" => RenameRule::Pascal,
            "camelCase" => RenameRule::Camel,
            "snake_case" => RenameRule::Snake,
            "SCREAMING_SNAKE_CASE" => RenameRule::ScreamingSnake,
            "kebab-case" => RenameRule::Kebab,
            "SCREAMING-KEBAB-CASE" => RenameRule::ScreamingKebab,
            other => {
                return Err(syn::Error::new(
                    span,
                    format!("unknown rename rule `{other}`"),
                ))
            }
        })
    }

    /// Applies the rule to a `snake_case` field name.
    pub fn apply_to_field(self, name: &str) -> String {
        match self {
            RenameRule::None => name.to_owned(),
            RenameRule::Lower | RenameRule::Snake => name.to_owned(),
            RenameRule::Upper | RenameRule::ScreamingSnake => name.to_ascii_uppercase(),
            RenameRule::Pascal => name.split('_').map(capitalise).collect::<Vec<_>>().concat(),
            RenameRule::Camel => {
                let pascal = RenameRule::Pascal.apply_to_field(name);
                let mut chars = pascal.chars();
                match chars.next() {
                    Some(first) => first.to_lowercase().chain(chars).collect(),
                    None => pascal,
                }
            }
            RenameRule::Kebab => name.replace('_', "-"),
            RenameRule::ScreamingKebab => name.replace('_', "-").to_ascii_uppercase(),
        }
    }

    /// Applies the rule to a `PascalCase` variant name.
    pub fn apply_to_variant(self, name: &str) -> String {
        match self {
            RenameRule::None | RenameRule::Pascal => name.to_owned(),
            RenameRule::Lower => name.to_ascii_lowercase(),
            RenameRule::Upper => name.to_ascii_uppercase(),
            RenameRule::Camel => {
                let mut chars = name.chars();
                match chars.next() {
                    Some(first) => first.to_lowercase().chain(chars).collect(),
                    None => name.to_owned(),
                }
            }
            RenameRule::Snake => pascal_to_snake(name),
            RenameRule::ScreamingSnake => pascal_to_snake(name).to_ascii_uppercase(),
            RenameRule::Kebab => pascal_to_snake(name).replace('_', "-"),
            RenameRule::ScreamingKebab => {
                pascal_to_snake(name).replace('_', "-").to_ascii_uppercase()
            }
        }
    }
}

fn capitalise(segment: &str) -> String {
    let mut chars = segment.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn pascal_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, ch) in name.char_indices() {
        if ch.is_uppercase() {
            if index != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Reads a string literal out of an attribute value.
pub fn lit_str(expr: &Expr) -> Result<String> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Ok(s.value()),
        other => Err(syn::Error::new(other.span(), "expected a string literal")),
    }
}

/// Reads an array of string literals, e.g. `tags = ["users", "admin"]`.
pub fn lit_str_array(expr: &Expr) -> Result<Vec<String>> {
    match expr {
        Expr::Array(array) => array.elems.iter().map(lit_str).collect(),
        // A bare string is accepted as a one-element list, because
        // `tags = "users"` is what everyone types first.
        other => lit_str(other).map(|s| vec![s]),
    }
}

/// True when the type is syntactically `Option<_>`.
///
/// This has to be a syntactic check — there is no way to ask the type system
/// whether a type is `Option` from a derive. A field aliased to `Option` under
/// another name will be treated as required, which is the safe direction.
pub fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else { return None };
    if path.qself.is_some() {
        return None;
    }
    let segment = path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}

/// Turns an attribute value into a `serde_json::Value` expression.
///
/// A string literal is always a JSON string and any other expression is
/// serialised, so `example = "42"` and `example = 42` mean different things
/// and neither is a guess. Use `example_json` to supply raw JSON text.
pub fn json_expr(expr: &Expr) -> proc_macro2::TokenStream {
    use quote::quote;
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => {
            let raw = s.value();
            quote!(::velo::__private::json_string(#raw))
        }
        other => quote!(::velo::__private::json_value(&#other)),
    }
}

/// Parses an attribute value as raw JSON text, for `example_json = "..."`.
pub fn json_literal_expr(expr: &Expr) -> Result<proc_macro2::TokenStream> {
    use quote::quote;
    let raw = lit_str(expr)?;
    // Rejecting malformed JSON here turns a documentation typo into a build
    // error instead of a confusing example in the published document.
    if let Err(error) = serde_json_check(&raw) {
        return Err(syn::Error::new(
            expr.span(),
            format!("`example_json` is not valid JSON: {error}"),
        ));
    }
    Ok(quote!(::velo::__private::json_from_str(#raw)))
}

/// A dependency-free structural check: balanced brackets and quotes.
///
/// The macro crate deliberately does not depend on `serde_json`, so this
/// catches the common typos rather than fully validating the grammar.
fn serde_json_check(raw: &str) -> std::result::Result<(), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("value is empty".into());
    }

    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for ch in trimmed.chars() {
        if in_string {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' | '[' => stack.push(ch),
            '}' => match stack.pop() {
                Some('{') => {}
                _ => return Err("unbalanced `}`".into()),
            },
            ']' => match stack.pop() {
                Some('[') => {}
                _ => return Err("unbalanced `]`".into()),
            },
            _ => {}
        }
    }

    if in_string {
        Err("unterminated string".into())
    } else if !stack.is_empty() {
        Err("unclosed `{` or `[`".into())
    } else {
        Ok(())
    }
}
