//! The routing attribute macros, and the `routes!` collector.

use crate::attr::{doc_comment, lit_str_array};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Expr, FnArg, GenericParam, ItemFn, LitStr, Result, ReturnType, Token, Type};

/// The suffix that turns a handler name into its generated route type.
pub const ROUTE_SUFFIX: &str = "__route";

/// `#[get("/path", tags = ["x"], summary = "...")]`
struct RouteArgs {
    path: LitStr,
    tags: Vec<String>,
    summary: Option<String>,
    description: Option<String>,
    operation_id: Option<String>,
    deprecated: bool,
    hidden: bool,
}

impl Parse for RouteArgs {
    fn parse(input: ParseStream) -> Result<Self> {
        let path: LitStr = input.parse().map_err(|_| {
            syn::Error::new(
                input.span(),
                "expected a route path, e.g. `#[get(\"/users/{id}\")]`",
            )
        })?;

        let mut args = RouteArgs {
            path,
            tags: Vec::new(),
            summary: None,
            description: None,
            operation_id: None,
            deprecated: false,
            hidden: false,
        };

        while input.peek(Token![,]) {
            let _: Token![,] = input.parse()?;
            if input.is_empty() {
                break;
            }
            let key: syn::Ident = input.parse()?;
            let name = key.to_string();

            match name.as_str() {
                "deprecated" => args.deprecated = true,
                "hidden" => args.hidden = true,
                _ => {
                    let _: Token![=] = input.parse().map_err(|_| {
                        syn::Error::new(key.span(), format!("`{name}` needs a value"))
                    })?;
                    let value: Expr = input.parse()?;
                    match name.as_str() {
                        "tags" => args.tags = lit_str_array(&value)?,
                        "summary" => args.summary = Some(crate::attr::lit_str(&value)?),
                        "description" => args.description = Some(crate::attr::lit_str(&value)?),
                        "operation_id" | "id" => {
                            args.operation_id = Some(crate::attr::lit_str(&value)?)
                        }
                        other => {
                            return Err(syn::Error::new(
                                key.span(),
                                format!(
                                    "unknown route option `{other}`; expected one of `tags`, \
                                     `summary`, `description`, `operation_id`, `deprecated`, \
                                     `hidden`"
                                ),
                            ))
                        }
                    }
                }
            }
        }

        Ok(args)
    }
}

pub fn expand(method: &str, attr: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let args: RouteArgs = syn::parse2(attr)?;
    let function: ItemFn = syn::parse2(item)?;

    check_signature(&function)?;
    validate_path(&args.path)?;

    let fn_ident = &function.sig.ident;
    let vis = &function.vis;
    let route_ident = format_ident!("{}{}", fn_ident, ROUTE_SUFFIX);
    let path = args.path.value();

    // Argument types drive both extraction and documentation.
    let arg_types: Vec<&Type> = function
        .sig
        .inputs
        .iter()
        .map(|arg| match arg {
            FnArg::Typed(typed) => Ok(&*typed.ty),
            FnArg::Receiver(receiver) => Err(syn::Error::new(
                receiver.span(),
                "route handlers cannot take `self`; they are free functions",
            )),
        })
        .collect::<Result<_>>()?;

    let bindings: Vec<syn::Ident> = (0..arg_types.len())
        .map(|i| format_ident!("__arg{}", i))
        .collect();

    let extraction = bindings.iter().zip(&arg_types).map(|(binding, ty)| {
        quote! {
            let #binding = match <#ty as ::velo::FromRequest>::from_request(&mut __req).await {
                ::core::result::Result::Ok(value) => value,
                ::core::result::Result::Err(rejection) => {
                    return ::velo::__private::IntoResponse::into_response(rejection);
                }
            };
        }
    });

    let return_type: Type = match &function.sig.output {
        ReturnType::Default => syn::parse_quote!(()),
        ReturnType::Type(_, ty) => (**ty).clone(),
    };

    // Doc comments become the operation's prose unless overridden.
    let docs = doc_comment(&function.attrs);
    let summary = args.summary.or(docs.summary);
    let description = args.description.or(docs.description);

    let set_summary = match &summary {
        Some(text) => quote!(__ctx.operation.summary = ::core::option::Option::Some(#text.into());),
        None => quote!(),
    };
    let set_description = match &description {
        Some(text) => {
            quote!(__ctx.operation.description = ::core::option::Option::Some(#text.into());)
        }
        None => quote!(),
    };
    let set_operation_id = match &args.operation_id {
        Some(id) => {
            quote!(__ctx.operation.operation_id = ::core::option::Option::Some(#id.into());)
        }
        None => quote!(),
    };
    let tags = &args.tags;
    let set_tags = quote!(#(__ctx.operation.tags.push(#tags.into());)*);
    let set_deprecated = args
        .deprecated
        .then(|| quote!(__ctx.operation.deprecated = true;));
    let set_hidden = args.hidden.then(|| {
        quote! {
            __ctx.operation.extensions.insert(
                "x-internal".into(),
                ::velo::__private::json_bool(true),
            );
        }
    });

    let describe_inputs = arg_types.iter().map(|ty| {
        quote! { <#ty as ::velo::__private::OperationInput>::describe(__ctx); }
    });

    let doc_note = format!(
        "Route definition for [`{fn_ident}`]: `{method} {path}`. Generated by `#[{}]`.",
        method.to_ascii_lowercase()
    );

    Ok(quote! {
        #function

        #[doc = #doc_note]
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #[derive(::core::clone::Clone, ::core::marker::Copy, ::core::fmt::Debug)]
        #vis struct #route_ident;

        impl ::velo::__private::IntoRoute for #route_ident {
            fn into_route(self) -> ::velo::__private::RouteDef {
                ::velo::__private::RouteDef {
                    method: #method,
                    path: #path,
                    handler: ::velo::__private::Arc::new(
                        |mut __req: ::velo::__private::Request| {
                            ::velo::__private::Box::pin(async move {
                                #(#extraction)*
                                ::velo::__private::IntoResponse::into_response(
                                    #fn_ident(#(#bindings),*).await
                                )
                            })
                        },
                    ),
                    describe: |__ctx: &mut ::velo::__private::OperationContext<'_>| {
                        #set_summary
                        #set_description
                        #set_operation_id
                        #set_tags
                        #set_deprecated
                        #set_hidden
                        #(#describe_inputs)*
                        <#return_type as ::velo::__private::OperationOutput>::describe(__ctx);
                    },
                }
            }
        }
    })
}

fn check_signature(function: &ItemFn) -> Result<()> {
    if function.sig.asyncness.is_none() {
        return Err(syn::Error::new(
            function.sig.span(),
            "route handlers must be `async fn`",
        ));
    }

    let has_type_params = function
        .sig
        .generics
        .params
        .iter()
        .any(|p| !matches!(p, GenericParam::Lifetime(_)));
    if has_type_params {
        return Err(syn::Error::new(
            function.sig.generics.span(),
            "route handlers cannot be generic: the OpenAPI operation is built from concrete \
             types, and there is no single operation to generate for a generic handler",
        ));
    }

    if let ReturnType::Type(_, ty) = &function.sig.output {
        if let Some(span) = find_impl_trait(ty) {
            return Err(syn::Error::new(
                span,
                "route handlers must name their return type; `impl Trait` cannot appear in it, \
                 because the generated operation has to refer to the type by name. Return a \
                 concrete type such as `Json<T>`, `Result<Json<T>, ApiError>`, or \
                 `Sse<EventStream>` (see `Sse::boxed`)",
            ));
        }
    }

    Ok(())
}

/// Finds `impl Trait` anywhere inside a type, including nested in generic
/// arguments — `Sse<impl Stream>` is just as unnameable as a bare `impl`.
fn find_impl_trait(ty: &Type) -> Option<proc_macro2::Span> {
    match ty {
        Type::ImplTrait(inner) => Some(inner.span()),
        Type::Path(path) => path.path.segments.iter().find_map(|segment| {
            let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
                return None;
            };
            args.args.iter().find_map(|arg| match arg {
                syn::GenericArgument::Type(inner) => find_impl_trait(inner),
                _ => None,
            })
        }),
        Type::Tuple(tuple) => tuple.elems.iter().find_map(find_impl_trait),
        Type::Reference(reference) => find_impl_trait(&reference.elem),
        Type::Paren(paren) => find_impl_trait(&paren.elem),
        Type::Group(group) => find_impl_trait(&group.elem),
        _ => None,
    }
}

/// Checks the template's braces balance and that names are usable.
fn validate_path(path: &LitStr) -> Result<()> {
    let raw = path.value();

    if !raw.starts_with('/') {
        return Err(syn::Error::new(
            path.span(),
            format!("route path must start with `/`, got `{raw}`"),
        ));
    }

    let mut depth = 0usize;
    let mut current = String::new();
    let mut seen: Vec<String> = Vec::new();

    for ch in raw.chars() {
        match ch {
            '{' => {
                if depth > 0 {
                    return Err(syn::Error::new(path.span(), "nested `{` in route path"));
                }
                depth += 1;
                current.clear();
            }
            '}' => {
                if depth == 0 {
                    return Err(syn::Error::new(path.span(), "unmatched `}` in route path"));
                }
                depth -= 1;
                let name = current.trim_start_matches('*');
                if name.is_empty() {
                    return Err(syn::Error::new(
                        path.span(),
                        "a path parameter needs a name, e.g. `{id}`",
                    ));
                }
                if seen.iter().any(|s| s == name) {
                    return Err(syn::Error::new(
                        path.span(),
                        format!("path parameter `{name}` appears more than once"),
                    ));
                }
                seen.push(name.to_owned());
            }
            _ if depth > 0 => current.push(ch),
            _ => {}
        }
    }

    if depth != 0 {
        return Err(syn::Error::new(path.span(), "unmatched `{` in route path"));
    }

    Ok(())
}

/// `routes![list_users, create_user]`
pub fn routes(input: TokenStream) -> Result<TokenStream> {
    if input.is_empty() {
        return Ok(quote! {
            ::std::vec::Vec::<::velo::__private::RouteDef>::new()
        });
    }

    let paths =
        syn::parse::Parser::parse2(Punctuated::<syn::Path, Token![,]>::parse_terminated, input)?;

    let entries = paths.iter().map(|path| {
        let mut rewritten = path.clone();
        // Rewrite only the final segment so `handlers::get_user` resolves to
        // `handlers::get_user__route`.
        if let Some(last) = rewritten.segments.last_mut() {
            last.ident = format_ident!("{}{}", last.ident, ROUTE_SUFFIX, span = last.ident.span());
        }
        quote!(::velo::__private::IntoRoute::into_route(#rewritten))
    });

    Ok(quote! {
        ::std::vec![#(#entries),*]
    })
}
