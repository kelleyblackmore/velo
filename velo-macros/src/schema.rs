//! `#[derive(Schema)]`: JSON Schema generation and validation from one
//! declaration.

use crate::attr::{doc_comment, json_expr, json_literal_expr, lit_str, option_inner, RenameRule};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DeriveInput, Expr, Fields, GenericParam, Ident, Path, Result, Type, Variant,
};

pub fn derive(input: DeriveInput) -> Result<TokenStream> {
    let container = Container::parse(&input)?;

    let schema_body = match &input.data {
        Data::Struct(data) => struct_schema(&container, &data.fields)?,
        Data::Enum(data) => enum_schema(&container, &data.variants)?,
        Data::Union(_) => {
            return Err(syn::Error::new(
                input.span(),
                "`Schema` cannot be derived for unions; they have no JSON representation",
            ))
        }
    };

    let validate_body = match &input.data {
        Data::Struct(data) => struct_validate(&container, &data.fields)?,
        Data::Enum(data) => enum_validate(&container, &data.variants)?,
        Data::Union(_) => unreachable!("rejected above"),
    };

    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    // Every type parameter must itself be describable for the container to be.
    let mut schema_where = where_clause
        .cloned()
        .unwrap_or_else(|| syn::parse_quote!(where));
    for param in &input.generics.params {
        if let GenericParam::Type(type_param) = param {
            let ident = &type_param.ident;
            schema_where
                .predicates
                .push(syn::parse_quote!(#ident: ::velo::__private::JsonSchema));
        }
    }

    let name_expr = container.name_expr(&input);

    Ok(quote! {
        impl #impl_generics ::velo::__private::JsonSchema for #ident #ty_generics #schema_where {
            fn schema_name() -> ::core::option::Option<::velo::__private::String> {
                #name_expr
            }

            fn json_schema(
                generator: &mut ::velo::__private::SchemaGenerator,
            ) -> ::velo::__private::Schema {
                #schema_body
            }
        }

        impl #impl_generics ::velo::__private::Validate for #ident #ty_generics #where_clause {
            fn validate(
                &self,
            ) -> ::core::result::Result<(), ::velo::__private::ValidationErrors> {
                #validate_body
            }
        }
    })
}

// ---------------------------------------------------------------------------
// container
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Container {
    rename: Option<String>,
    rename_all: RenameRule,
    rename_all_variants: Option<RenameRule>,
    inline: bool,
    deny_unknown_fields: bool,
    transparent: bool,
    title: Option<String>,
    description: Option<String>,
    example: Option<TokenStream>,
    tag: Option<String>,
    content: Option<String>,
    untagged: bool,
}

impl Container {
    fn parse(input: &DeriveInput) -> Result<Self> {
        let mut container = Container {
            description: doc_comment(&input.attrs)
                .description
                .or_else(|| doc_comment(&input.attrs).summary),
            ..Default::default()
        };

        for attr in &input.attrs {
            if attr.path().is_ident("serde") {
                container.parse_serde(attr)?;
            } else if attr.path().is_ident("schema") {
                container.parse_schema(attr)?;
            }
        }
        Ok(container)
    }

    fn parse_serde(&mut self, attr: &Attribute) -> Result<()> {
        attr.parse_nested_meta(|meta| {
            let path = meta.path.clone();
            if path.is_ident("rename_all") {
                let value: syn::LitStr = meta.value()?.parse()?;
                self.rename_all = RenameRule::parse(&value.value(), value.span())?;
            } else if path.is_ident("rename_all_fields") {
                let value: syn::LitStr = meta.value()?.parse()?;
                self.rename_all_variants = Some(RenameRule::parse(&value.value(), value.span())?);
            } else if path.is_ident("rename") {
                let value: syn::LitStr = meta.value()?.parse()?;
                self.rename = Some(value.value());
            } else if path.is_ident("deny_unknown_fields") {
                self.deny_unknown_fields = true;
            } else if path.is_ident("transparent") {
                self.transparent = true;
            } else if path.is_ident("tag") {
                let value: syn::LitStr = meta.value()?.parse()?;
                self.tag = Some(value.value());
            } else if path.is_ident("content") {
                let value: syn::LitStr = meta.value()?.parse()?;
                self.content = Some(value.value());
            } else if path.is_ident("untagged") {
                self.untagged = true;
            } else if meta.input.peek(syn::Token![=]) {
                // Unknown `serde` keys are not ours to validate; skip the value
                // so parsing continues rather than failing the whole derive.
                let _: Expr = meta.value()?.parse()?;
            } else if meta.input.peek(syn::token::Paren) {
                let _: proc_macro2::Group = meta.input.parse()?;
            }
            Ok(())
        })
    }

    fn parse_schema(&mut self, attr: &Attribute) -> Result<()> {
        attr.parse_nested_meta(|meta| {
            let path = meta.path.clone();
            if path.is_ident("rename") {
                self.rename = Some(lit_str(&meta.value()?.parse()?)?);
            } else if path.is_ident("inline") {
                self.inline = true;
            } else if path.is_ident("title") {
                self.title = Some(lit_str(&meta.value()?.parse()?)?);
            } else if path.is_ident("description") {
                self.description = Some(lit_str(&meta.value()?.parse()?)?);
            } else if path.is_ident("example") {
                self.example = Some(json_expr(&meta.value()?.parse()?));
            } else if path.is_ident("example_json") {
                self.example = Some(json_literal_expr(&meta.value()?.parse()?)?);
            } else if path.is_ident("deny_unknown_fields") {
                self.deny_unknown_fields = true;
            } else {
                return Err(meta.error(
                    "unknown `schema` option; expected one of `rename`, `inline`, `title`, \
                     `description`, `example`, `deny_unknown_fields`",
                ));
            }
            Ok(())
        })
    }

    /// The component name, or `None` when the schema should be inlined.
    fn name_expr(&self, input: &DeriveInput) -> TokenStream {
        if self.inline {
            return quote!(::core::option::Option::None);
        }

        let base = self
            .rename
            .clone()
            .unwrap_or_else(|| input.ident.to_string());

        let params: Vec<&Ident> = input
            .generics
            .params
            .iter()
            .filter_map(|p| match p {
                GenericParam::Type(t) => Some(&t.ident),
                _ => None,
            })
            .collect();

        if params.is_empty() {
            quote!(::core::option::Option::Some(#base.into()))
        } else {
            // `Page<User>` registers as `Page_User`, so two instantiations do
            // not collide under one name.
            quote! {
                ::core::option::Option::Some({
                    let mut name = ::velo::__private::String::from(#base);
                    #(
                        name.push('_');
                        name.push_str(&::velo::__private::name_of::<#params>());
                    )*
                    name
                })
            }
        }
    }

    fn annotations(&self) -> TokenStream {
        let title = option_str(&self.title);
        let description = option_str(&self.description);
        let example = match &self.example {
            Some(value) => quote!(schema.examples.push(#value);),
            None => quote!(),
        };
        let deny = if self.deny_unknown_fields {
            quote! {
                schema.additional_properties =
                    ::core::option::Option::Some(::velo::__private::AdditionalProperties::Bool(false));
            }
        } else {
            quote!()
        };
        quote! {
            schema.title = #title;
            if schema.description.is_none() {
                schema.description = #description;
            }
            #example
            #deny
        }
    }
}

fn option_str(value: &Option<String>) -> TokenStream {
    match value {
        Some(text) => quote!(::core::option::Option::Some(#text.into())),
        None => quote!(::core::option::Option::None),
    }
}

// ---------------------------------------------------------------------------
// fields
// ---------------------------------------------------------------------------

struct Field {
    ident: Option<Ident>,
    index: usize,
    ty: Type,
    name: String,
    skip: bool,
    flatten: bool,
    has_default: bool,
    description: Option<String>,
    example: Option<TokenStream>,
    default_value: Option<TokenStream>,
    read_only: bool,
    write_only: bool,
    deprecated: bool,
    rules: Rules,
}

#[derive(Default)]
struct Rules {
    min_length: Option<Expr>,
    max_length: Option<Expr>,
    min_items: Option<Expr>,
    max_items: Option<Expr>,
    minimum: Option<Expr>,
    maximum: Option<Expr>,
    exclusive_minimum: Option<Expr>,
    exclusive_maximum: Option<Expr>,
    multiple_of: Option<Expr>,
    pattern: Option<String>,
    format: Option<String>,
    non_blank: bool,
    nested: bool,
    custom: Vec<Path>,
}

impl Rules {
    fn is_empty(&self) -> bool {
        self.min_length.is_none()
            && self.max_length.is_none()
            && self.min_items.is_none()
            && self.max_items.is_none()
            && self.minimum.is_none()
            && self.maximum.is_none()
            && self.exclusive_minimum.is_none()
            && self.exclusive_maximum.is_none()
            && self.multiple_of.is_none()
            && self.pattern.is_none()
            && self.format.is_none()
            && !self.non_blank
            && !self.nested
            && self.custom.is_empty()
    }

    fn parse(attr: &Attribute) -> Result<Self> {
        let mut rules = Rules::default();
        attr.parse_nested_meta(|meta| {
            let path = meta.path.clone();
            let numeric = |slot: &mut Option<Expr>| -> Result<()> {
                *slot = Some(meta.value()?.parse()?);
                Ok(())
            };

            if path.is_ident("min_length") {
                numeric(&mut rules.min_length)?;
            } else if path.is_ident("max_length") {
                numeric(&mut rules.max_length)?;
            } else if path.is_ident("min_items") {
                numeric(&mut rules.min_items)?;
            } else if path.is_ident("max_items") {
                numeric(&mut rules.max_items)?;
            } else if path.is_ident("minimum") {
                numeric(&mut rules.minimum)?;
            } else if path.is_ident("maximum") {
                numeric(&mut rules.maximum)?;
            } else if path.is_ident("exclusive_minimum") {
                numeric(&mut rules.exclusive_minimum)?;
            } else if path.is_ident("exclusive_maximum") {
                numeric(&mut rules.exclusive_maximum)?;
            } else if path.is_ident("multiple_of") {
                numeric(&mut rules.multiple_of)?;
            } else if path.is_ident("range") {
                // `range(min = 1, max = 10)` is the shorthand people reach for.
                meta.parse_nested_meta(|inner| {
                    if inner.path.is_ident("min") {
                        rules.minimum = Some(inner.value()?.parse()?);
                    } else if inner.path.is_ident("max") {
                        rules.maximum = Some(inner.value()?.parse()?);
                    } else {
                        return Err(inner.error("expected `min` or `max`"));
                    }
                    Ok(())
                })?;
            } else if path.is_ident("pattern") {
                rules.pattern = Some(lit_str(&meta.value()?.parse()?)?);
            } else if path.is_ident("format") {
                rules.format = Some(lit_str(&meta.value()?.parse()?)?);
            } else if path.is_ident("non_blank") {
                rules.non_blank = true;
            } else if path.is_ident("nested") {
                rules.nested = true;
            } else if path.is_ident("custom") {
                let value: syn::LitStr = meta.value()?.parse()?;
                rules.custom.push(value.parse()?);
            } else {
                return Err(meta.error(
                    "unknown `validate` rule; expected one of `min_length`, `max_length`, \
                     `min_items`, `max_items`, `minimum`, `maximum`, `exclusive_minimum`, \
                     `exclusive_maximum`, `multiple_of`, `range`, `pattern`, `format`, \
                     `non_blank`, `nested`, `custom`",
                ));
            }
            Ok(())
        })?;
        Ok(rules)
    }

    /// The JSON Schema keywords these rules imply.
    fn keywords(&self) -> TokenStream {
        let mut out = TokenStream::new();
        let mut set = |name: &str, expr: Option<&Expr>, cast: TokenStream| {
            if let Some(expr) = expr {
                let field = format_ident!("{}", name);
                out.extend(quote! {
                    field_schema.#field = ::core::option::Option::Some((#expr) as #cast);
                });
            }
        };
        set("min_length", self.min_length.as_ref(), quote!(u64));
        set("max_length", self.max_length.as_ref(), quote!(u64));
        set("min_items", self.min_items.as_ref(), quote!(u64));
        set("max_items", self.max_items.as_ref(), quote!(u64));
        set("minimum", self.minimum.as_ref(), quote!(f64));
        set("maximum", self.maximum.as_ref(), quote!(f64));
        set(
            "exclusive_minimum",
            self.exclusive_minimum.as_ref(),
            quote!(f64),
        );
        set(
            "exclusive_maximum",
            self.exclusive_maximum.as_ref(),
            quote!(f64),
        );
        set("multiple_of", self.multiple_of.as_ref(), quote!(f64));

        if let Some(pattern) = &self.pattern {
            out.extend(quote! {
                field_schema.pattern = ::core::option::Option::Some(#pattern.into());
            });
        }
        if let Some(format) = &self.format {
            out.extend(quote! {
                field_schema.format = ::core::option::Option::Some(#format.into());
            });
        }
        if self.non_blank && self.min_length.is_none() {
            out.extend(quote! {
                field_schema.min_length = ::core::option::Option::Some(1);
            });
        }
        out
    }

    /// The runtime checks these rules imply, against a binding named `value`.
    fn checks(&self, pointer: &str) -> TokenStream {
        let mut out = TokenStream::new();
        let rules = quote!(::velo::__private::rules);

        macro_rules! emit {
            ($slot:expr, $func:ident, $cast:tt) => {
                if let Some(expr) = &$slot {
                    let func = format_ident!("{}", stringify!($func));
                    let cast = format_ident!("{}", stringify!($cast));
                    out.extend(quote! {
                        #rules::#func(value, (#expr) as #cast, #pointer, &mut errors);
                    });
                }
            };
        }

        emit!(self.min_length, min_length, usize);
        emit!(self.max_length, max_length, usize);
        emit!(self.min_items, min_items, usize);
        emit!(self.max_items, max_items, usize);
        emit!(self.minimum, minimum, f64);
        emit!(self.maximum, maximum, f64);
        emit!(self.exclusive_minimum, exclusive_minimum, f64);
        emit!(self.exclusive_maximum, exclusive_maximum, f64);
        emit!(self.multiple_of, multiple_of, f64);

        if self.non_blank {
            out.extend(quote! {
                #rules::non_blank(value.as_ref(), #pointer, &mut errors);
            });
        }
        if let Some(format) = &self.format {
            out.extend(quote! {
                #rules::format(value.as_ref(), #format, #pointer, &mut errors);
            });
        }
        if let Some(pattern) = &self.pattern {
            // Compiled once per process rather than once per request.
            out.extend(quote! {
                {
                    static PATTERN: ::velo::__private::OnceLock<::velo::__private::Regex> =
                        ::velo::__private::OnceLock::new();
                    let compiled = PATTERN.get_or_init(|| {
                        ::velo::__private::Regex::new(#pattern)
                            .expect(concat!("`#[validate(pattern = ", #pattern, ")]` is not a valid regex"))
                    });
                    #rules::pattern_compiled(value.as_ref(), compiled, #pointer, &mut errors);
                }
            });
        }
        if self.nested {
            out.extend(quote! {
                if let ::core::result::Result::Err(nested) =
                    ::velo::__private::Validate::validate(value)
                {
                    errors.merge_at(#pointer, nested);
                }
            });
        }
        for custom in &self.custom {
            out.extend(quote! {
                if let ::core::result::Result::Err(message) = #custom(value) {
                    errors.push(#pointer, "custom", message);
                }
            });
        }
        out
    }
}

impl Field {
    fn parse(index: usize, field: &syn::Field, rename_all: RenameRule) -> Result<Self> {
        let ident = field.ident.clone();
        let mut parsed = Field {
            name: match &ident {
                Some(ident) => rename_all.apply_to_field(&ident.to_string()),
                None => index.to_string(),
            },
            ident,
            index,
            ty: field.ty.clone(),
            skip: false,
            flatten: false,
            has_default: false,
            description: doc_comment(&field.attrs).summary.map(|s| {
                match doc_comment(&field.attrs).description {
                    Some(rest) => format!("{s}\n\n{rest}"),
                    None => s,
                }
            }),
            example: None,
            default_value: None,
            read_only: false,
            write_only: false,
            deprecated: false,
            rules: Rules::default(),
        };

        for attr in &field.attrs {
            if attr.path().is_ident("serde") {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("rename") {
                        let value: syn::LitStr = meta.value()?.parse()?;
                        parsed.name = value.value();
                    } else if meta.path.is_ident("skip")
                        || meta.path.is_ident("skip_serializing")
                        || meta.path.is_ident("skip_deserializing")
                    {
                        parsed.skip = true;
                    } else if meta.path.is_ident("flatten") {
                        parsed.flatten = true;
                    } else if meta.path.is_ident("default") {
                        parsed.has_default = true;
                        if meta.input.peek(syn::Token![=]) {
                            let _: Expr = meta.value()?.parse()?;
                        }
                    } else if meta.input.peek(syn::Token![=]) {
                        let _: Expr = meta.value()?.parse()?;
                    } else if meta.input.peek(syn::token::Paren) {
                        let _: proc_macro2::Group = meta.input.parse()?;
                    }
                    Ok(())
                })?;
            } else if attr.path().is_ident("schema") {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("description") {
                        parsed.description = Some(lit_str(&meta.value()?.parse()?)?);
                    } else if meta.path.is_ident("example") {
                        parsed.example = Some(json_expr(&meta.value()?.parse()?));
                    } else if meta.path.is_ident("example_json") {
                        parsed.example = Some(json_literal_expr(&meta.value()?.parse()?)?);
                    } else if meta.path.is_ident("default") {
                        parsed.default_value = Some(json_expr(&meta.value()?.parse()?));
                    } else if meta.path.is_ident("read_only") {
                        parsed.read_only = true;
                    } else if meta.path.is_ident("write_only") {
                        parsed.write_only = true;
                    } else if meta.path.is_ident("deprecated") {
                        parsed.deprecated = true;
                    } else if meta.path.is_ident("rename") {
                        parsed.name = lit_str(&meta.value()?.parse()?)?;
                    } else {
                        return Err(meta.error(
                            "unknown `schema` option on a field; expected one of `description`, \
                             `example`, `default`, `read_only`, `write_only`, `deprecated`, \
                             `rename`",
                        ));
                    }
                    Ok(())
                })?;
            } else if attr.path().is_ident("validate") {
                parsed.rules = Rules::parse(attr)?;
            }
        }

        Ok(parsed)
    }

    /// The expression that reads this field from `self`.
    fn accessor(&self) -> TokenStream {
        match &self.ident {
            Some(ident) => quote!(self.#ident),
            None => {
                let index = syn::Index::from(self.index);
                quote!(self.#index)
            }
        }
    }

    fn pointer(&self) -> String {
        format!("/{}", self.name.replace('~', "~0").replace('/', "~1"))
    }

    /// Builds the property schema for this field.
    fn schema(&self) -> TokenStream {
        let ty = &self.ty;
        let keywords = self.rules.keywords();
        let description = option_str(&self.description);
        let example = match &self.example {
            Some(value) => quote!(field_schema = field_schema.with_example(#value);),
            None => quote!(),
        };
        let default = match &self.default_value {
            Some(value) => {
                quote!(field_schema.default = ::core::option::Option::Some(#value);)
            }
            None => quote!(),
        };
        let read_only = self
            .read_only
            .then(|| quote!(field_schema.read_only = true;));
        let write_only = self
            .write_only
            .then(|| quote!(field_schema.write_only = true;));
        let deprecated = self
            .deprecated
            .then(|| quote!(field_schema.deprecated = true;));

        // Only a field that actually carries annotations needs unwrapping; a
        // plain reference stays a plain `$ref` so the document reads cleanly.
        let has_annotations = !keywords.is_empty()
            || self.description.is_some()
            || self.example.is_some()
            || self.default_value.is_some()
            || self.read_only
            || self.write_only
            || self.deprecated;

        let unwrap_ref = has_annotations.then(|| {
            quote! {
                // Keywords cannot sit beside a `$ref`, so a referenced type is
                // wrapped in `allOf` and the annotations go on the wrapper.
                if field_schema.is_pure_ref() {
                    let inner = field_schema;
                    field_schema = ::velo::__private::Schema {
                        all_of: ::std::vec![inner],
                        ..::core::default::Default::default()
                    };
                }
            }
        });

        quote! {{
            #[allow(unused_mut)]
            let mut field_schema = generator.subschema_for::<#ty>();
            #unwrap_ref
            #keywords
            if let ::core::option::Option::Some(text) = #description {
                field_schema.description = ::core::option::Option::Some(text);
            }
            #default
            #example
            #read_only
            #write_only
            #deprecated
            field_schema
        }}
    }
}

// ---------------------------------------------------------------------------
// structs
// ---------------------------------------------------------------------------

fn collect_fields(fields: &Fields, rename_all: RenameRule) -> Result<Vec<Field>> {
    fields
        .iter()
        .enumerate()
        .map(|(index, field)| Field::parse(index, field, rename_all))
        .filter(|field| field.as_ref().map(|f| !f.skip).unwrap_or(true))
        .collect()
}

fn struct_schema(container: &Container, fields: &Fields) -> Result<TokenStream> {
    let parsed = collect_fields(fields, container.rename_all)?;
    let annotations = container.annotations();

    // A newtype is transparent by default: `struct UserId(u64)` should
    // document as an integer, not an object wrapping one.
    if matches!(fields, Fields::Unnamed(f) if f.unnamed.len() == 1) || container.transparent {
        if let Some(field) = parsed.first() {
            let inner = field.schema();
            return Ok(quote! {
                #[allow(unused_mut)]
                let mut schema = #inner;
                #annotations
                schema
            });
        }
    }

    if matches!(fields, Fields::Unit) {
        return Ok(quote! {
            #[allow(unused_mut)]
            let mut schema = ::velo::__private::Schema::of_type("null");
            #annotations
            schema
        });
    }

    if matches!(fields, Fields::Unnamed(_)) {
        let items: Vec<TokenStream> = parsed.iter().map(Field::schema).collect();
        let len = items.len() as u64;
        return Ok(quote! {
            #[allow(unused_mut)]
            let mut schema = ::velo::__private::Schema::of_type("array");
            schema.prefix_items = ::std::vec![#(#items),*];
            schema.min_items = ::core::option::Option::Some(#len);
            schema.max_items = ::core::option::Option::Some(#len);
            #annotations
            schema
        });
    }

    let mut body = TokenStream::new();
    for field in &parsed {
        let name = &field.name;
        let ty = &field.ty;

        if field.flatten {
            // A flattened field contributes its own properties to this object.
            body.extend(quote! {{
                let inner = generator.inline_for::<#ty>();
                for (key, value) in inner.properties {
                    schema.properties.insert(key, value);
                }
                schema.required.extend(inner.required);
            }});
            continue;
        }

        let property = field.schema();
        let required = if field.has_default {
            quote!()
        } else {
            quote! {
                if !<#ty as ::velo::__private::JsonSchema>::OPTIONAL {
                    schema.required.push(#name.into());
                }
            }
        };
        body.extend(quote! {
            schema.properties.insert(#name.into(), #property);
            #required
        });
    }

    Ok(quote! {
        #[allow(unused_mut)]
        let mut schema = ::velo::__private::Schema::of_type("object");
        #body
        #annotations
        schema
    })
}

fn struct_validate(container: &Container, fields: &Fields) -> Result<TokenStream> {
    let parsed = collect_fields(fields, container.rename_all)?;
    let mut body = TokenStream::new();

    for field in &parsed {
        if field.rules.is_empty() {
            continue;
        }
        let pointer = field.pointer();
        let checks = field.rules.checks(&pointer);
        let accessor = field.accessor();

        // `None` is vacuously valid: a constraint describes the value when one
        // is present, and absence is what `Option` already models.
        if option_inner(&field.ty).is_some() {
            body.extend(quote! {
                if let ::core::option::Option::Some(value) = &#accessor {
                    #checks
                }
            });
        } else {
            body.extend(quote! {{
                let value = &#accessor;
                #checks
            }});
        }
    }

    if body.is_empty() {
        return Ok(quote!(::core::result::Result::Ok(())));
    }

    Ok(quote! {
        #[allow(unused_mut)]
        let mut errors = ::velo::__private::ValidationErrors::new();
        #body
        errors.into_result()
    })
}

// ---------------------------------------------------------------------------
// enums
// ---------------------------------------------------------------------------

/// How serde will represent this enum on the wire.
enum Tagging {
    External,
    Internal(String),
    Adjacent(String, String),
    Untagged,
}

impl Container {
    fn tagging(&self) -> Tagging {
        match (&self.tag, &self.content, self.untagged) {
            (_, _, true) => Tagging::Untagged,
            (Some(tag), Some(content), _) => Tagging::Adjacent(tag.clone(), content.clone()),
            (Some(tag), None, _) => Tagging::Internal(tag.clone()),
            _ => Tagging::External,
        }
    }

    fn variant_name(&self, variant: &Variant) -> Result<String> {
        for attr in &variant.attrs {
            if attr.path().is_ident("serde") {
                let mut renamed = None;
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("rename") {
                        let value: syn::LitStr = meta.value()?.parse()?;
                        renamed = Some(value.value());
                    } else if meta.input.peek(syn::Token![=]) {
                        let _: Expr = meta.value()?.parse()?;
                    } else if meta.input.peek(syn::token::Paren) {
                        let _: proc_macro2::Group = meta.input.parse()?;
                    }
                    Ok(())
                })?;
                if let Some(renamed) = renamed {
                    return Ok(renamed);
                }
            }
        }
        Ok(self.rename_all.apply_to_variant(&variant.ident.to_string()))
    }
}

fn enum_schema(
    container: &Container,
    variants: &syn::punctuated::Punctuated<Variant, syn::Token![,]>,
) -> Result<TokenStream> {
    let annotations = container.annotations();
    let tagging = container.tagging();
    let all_unit = variants.iter().all(|v| matches!(v.fields, Fields::Unit));

    // The common case by far: a closed set of string values.
    if all_unit && !matches!(tagging, Tagging::Untagged) {
        let names: Vec<String> = variants
            .iter()
            .map(|v| container.variant_name(v))
            .collect::<Result<_>>()?;
        let descriptions: Vec<TokenStream> = variants
            .iter()
            .map(|v| option_str(&doc_comment(&v.attrs).summary))
            .collect();

        return Ok(quote! {
            #[allow(unused_mut)]
            let mut schema = ::velo::__private::Schema::of_type("string");
            schema.enum_values = ::std::vec![
                #(::velo::__private::json_string(#names)),*
            ];
            // Per-variant prose has nowhere to live in a plain string enum, so
            // it is emitted as an extension that tooling can surface.
            let mut variant_docs: ::std::vec::Vec<::velo::__private::Value> =
                ::std::vec::Vec::new();
            #({
                let description: ::core::option::Option<::velo::__private::String> =
                    #descriptions;
                variant_docs.push(match description {
                    ::core::option::Option::Some(text) =>
                        ::velo::__private::json_string(&text),
                    ::core::option::Option::None => ::velo::__private::json_null(),
                });
            })*
            if variant_docs.iter().any(|d| !d.is_null()) {
                schema.extensions.insert(
                    "x-enum-descriptions".into(),
                    ::velo::__private::json_array(variant_docs),
                );
            }
            #annotations
            schema
        });
    }

    let mut arms = Vec::new();
    for variant in variants {
        let name = container.variant_name(variant)?;
        let payload = variant_payload_schema(container, variant)?;

        let arm = match (&tagging, &variant.fields) {
            (Tagging::Untagged, Fields::Unit) => quote!(::velo::__private::Schema::of_type("null")),
            (Tagging::Untagged, _) => payload,
            (Tagging::External, Fields::Unit) => quote! {{
                let mut s = ::velo::__private::Schema::of_type("string");
                s.const_value =
                    ::core::option::Option::Some(::velo::__private::json_string(#name));
                s
            }},
            (Tagging::External, _) => quote! {{
                let mut s = ::velo::__private::Schema::of_type("object");
                s.properties.insert(#name.into(), #payload);
                s.required = ::std::vec![#name.into()];
                s
            }},
            (Tagging::Internal(tag), Fields::Unit) => quote! {{
                let mut s = ::velo::__private::Schema::of_type("object");
                s.properties.insert(#tag.into(), ::velo::__private::const_string(#name));
                s.required = ::std::vec![#tag.into()];
                s
            }},
            (Tagging::Internal(tag), _) => quote! {{
                let mut discriminant = ::velo::__private::Schema::of_type("object");
                discriminant
                    .properties
                    .insert(#tag.into(), ::velo::__private::const_string(#name));
                discriminant.required = ::std::vec![#tag.into()];
                ::velo::__private::Schema {
                    all_of: ::std::vec![discriminant, #payload],
                    ..::core::default::Default::default()
                }
            }},
            (Tagging::Adjacent(tag, content), Fields::Unit) => quote! {{
                let mut s = ::velo::__private::Schema::of_type("object");
                s.properties.insert(#tag.into(), ::velo::__private::const_string(#name));
                s.required = ::std::vec![#tag.into()];
                let _ = #content;
                s
            }},
            (Tagging::Adjacent(tag, content), _) => quote! {{
                let mut s = ::velo::__private::Schema::of_type("object");
                s.properties.insert(#tag.into(), ::velo::__private::const_string(#name));
                s.properties.insert(#content.into(), #payload);
                s.required = ::std::vec![#tag.into(), #content.into()];
                s
            }},
        };
        arms.push(arm);
    }

    let combinator = match tagging {
        // Untagged variants are genuinely ambiguous, so `anyOf` is honest
        // where `oneOf` would claim exactly-one-matches.
        Tagging::Untagged => quote!(any_of),
        _ => quote!(one_of),
    };

    let discriminator = match container.tagging() {
        Tagging::Internal(tag) | Tagging::Adjacent(tag, _) => quote! {
            schema.discriminator = ::core::option::Option::Some(
                ::velo::__private::Discriminator {
                    property_name: #tag.into(),
                    mapping: ::core::default::Default::default(),
                },
            );
        },
        _ => quote!(),
    };

    Ok(quote! {
        #[allow(unused_mut)]
        let mut schema = ::velo::__private::Schema::default();
        schema.#combinator = ::std::vec![#(#arms),*];
        #discriminator
        #annotations
        schema
    })
}

/// The schema for a variant's payload, ignoring how it is tagged.
fn variant_payload_schema(container: &Container, variant: &Variant) -> Result<TokenStream> {
    let rename_all = container.rename_all_variants.unwrap_or(RenameRule::None);
    let fields = collect_fields(&variant.fields, rename_all)?;

    Ok(match &variant.fields {
        Fields::Unit => quote!(::velo::__private::Schema::of_type("null")),
        Fields::Unnamed(unnamed) if unnamed.unnamed.len() == 1 => {
            let field = &fields[0];
            field.schema()
        }
        Fields::Unnamed(_) => {
            let items: Vec<TokenStream> = fields.iter().map(Field::schema).collect();
            let len = items.len() as u64;
            quote! {{
                let mut s = ::velo::__private::Schema::of_type("array");
                s.prefix_items = ::std::vec![#(#items),*];
                s.min_items = ::core::option::Option::Some(#len);
                s.max_items = ::core::option::Option::Some(#len);
                s
            }}
        }
        Fields::Named(_) => {
            let mut body = TokenStream::new();
            for field in &fields {
                let name = &field.name;
                let ty = &field.ty;
                let property = field.schema();
                body.extend(quote! {
                    s.properties.insert(#name.into(), #property);
                    if !<#ty as ::velo::__private::JsonSchema>::OPTIONAL {
                        s.required.push(#name.into());
                    }
                });
            }
            quote! {{
                #[allow(unused_mut)]
                let mut s = ::velo::__private::Schema::of_type("object");
                #body
                s
            }}
        }
    })
}

fn enum_validate(
    container: &Container,
    variants: &syn::punctuated::Punctuated<Variant, syn::Token![,]>,
) -> Result<TokenStream> {
    let rename_all = container.rename_all_variants.unwrap_or(RenameRule::None);
    let mut arms = Vec::new();
    let mut any_rules = false;

    for variant in variants {
        let ident = &variant.ident;
        let fields = collect_fields(&variant.fields, rename_all)?;

        let (pattern, checks) = match &variant.fields {
            Fields::Unit => (quote!(Self::#ident), TokenStream::new()),
            Fields::Named(_) => {
                let bindings: Vec<&Ident> =
                    fields.iter().filter_map(|f| f.ident.as_ref()).collect();
                let mut checks = TokenStream::new();
                for field in &fields {
                    if field.rules.is_empty() {
                        continue;
                    }
                    any_rules = true;
                    let binding = field.ident.as_ref().expect("named field");
                    let pointer = field.pointer();
                    let rule_checks = field.rules.checks(&pointer);
                    checks.extend(bind_and_check(quote!(#binding), &field.ty, rule_checks));
                }
                (quote!(Self::#ident { #(#bindings,)* .. }), checks)
            }
            Fields::Unnamed(unnamed) => {
                let bindings: Vec<Ident> = (0..unnamed.unnamed.len())
                    .map(|i| format_ident!("__field{}", i))
                    .collect();
                let mut checks = TokenStream::new();
                for field in &fields {
                    if field.rules.is_empty() {
                        continue;
                    }
                    any_rules = true;
                    let binding = &bindings[field.index];
                    let pointer = format!("/{}", field.index);
                    let rule_checks = field.rules.checks(&pointer);
                    checks.extend(bind_and_check(quote!(#binding), &field.ty, rule_checks));
                }
                (quote!(Self::#ident(#(#bindings,)*)), checks)
            }
        };

        arms.push(quote! {
            #pattern => { #checks }
        });
    }

    if !any_rules {
        return Ok(quote!(::core::result::Result::Ok(())));
    }

    Ok(quote! {
        #[allow(unused_mut, unused_variables)]
        let mut errors = ::velo::__private::ValidationErrors::new();
        match self {
            #(#arms)*
        }
        errors.into_result()
    })
}

fn bind_and_check(binding: TokenStream, ty: &Type, checks: TokenStream) -> TokenStream {
    if option_inner(ty).is_some() {
        quote! {
            if let ::core::option::Option::Some(value) = #binding {
                #checks
            }
        }
    } else {
        quote! {{
            let value = #binding;
            #checks
        }}
    }
}
