use std::collections::HashSet;

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Data, DeriveInput, Expr, Fields, Lit, LitInt, LitStr, Meta, Path, Token, Type,
    parse_macro_input, punctuated::Punctuated, spanned::Spanned,
};

#[proc_macro_derive(ConfigSchema, attributes(config))]
pub fn derive_config_schema(input: TokenStream) -> TokenStream {
    expand_config_schema(parse_macro_input!(input as DeriveInput))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[proc_macro_derive(WorkflowValues, attributes(workflow_values, value))]
pub fn derive_workflow_values(input: TokenStream) -> TokenStream {
    expand_workflow_values(parse_macro_input!(input as DeriveInput))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_workflow_values(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "WorkflowValues does not support generic types",
        ));
    }
    reject_serde_rename_all(&input.attrs)?;
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            input.ident.span(),
            "WorkflowValues requires a struct",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new(
            data.fields.span(),
            "WorkflowValues requires named fields",
        ));
    };

    let mut event = None;
    for attribute in &input.attrs {
        if attribute.path().is_ident("workflow_values") {
            attribute.parse_nested_meta(|meta| {
                if meta.path.is_ident("event") {
                    event = Some(meta.value()?.parse::<LitStr>()?);
                    Ok(())
                } else {
                    Err(meta.error("supported key is `event`"))
                }
            })?;
        }
    }
    let mut ids = HashSet::new();
    let mut descriptors = Vec::new();
    let mut values = Vec::new();
    if let Some(event) = event {
        ids.insert("event".to_owned());
        descriptors.push(quote! {
            ::snenk_bot::schema::ConfigOutput {
                id: "event", label: "Event", description: "",
                kind: ::snenk_bot::schema::ConfigOutputKind::Text, required: true,
            }
        });
        values.push(quote! { ("event".to_owned(), ::serde_json::json!(#event)) });
    }
    for field in &fields.named {
        let name = field.ident.as_ref().expect("named fields were checked");
        let mut label = None;
        let mut description = None;
        let mut secret = false;
        for attribute in &field.attrs {
            if attribute.path().is_ident("value") {
                attribute.parse_nested_meta(|meta| {
                    if meta.path.is_ident("label") {
                        label = Some(meta.value()?.parse::<LitStr>()?);
                    } else if meta.path.is_ident("description") {
                        description = Some(meta.value()?.parse::<LitStr>()?);
                    } else if meta.path.is_ident("secret") {
                        secret = true;
                    } else {
                        return Err(
                            meta.error("supported keys are `label`, `description`, and `secret`")
                        );
                    }
                    Ok(())
                })?;
            }
        }
        if secret {
            if label.is_some() || description.is_some() {
                return Err(syn::Error::new(
                    field.span(),
                    "secret values cannot have public labels",
                ));
            }
            continue;
        }
        let label = required(label, field.span(), "missing field `value(label = \"…\")`")?;
        let id = serde_field_name(field)?;
        if id.is_empty() || !ids.insert(id.clone()) {
            return Err(syn::Error::new(
                field.span(),
                "duplicate or empty workflow value ID",
            ));
        }
        let description = description.unwrap_or_else(|| LitStr::new("", field.span()));
        let (inner, required) = optional_inner_type(&field.ty)?;
        let kind = value_kind(inner)?;
        descriptors.push(quote! {
            ::snenk_bot::schema::ConfigOutput {
                id: #id, label: #label, description: #description,
                kind: #kind, required: #required,
            }
        });
        values.push(quote! { (#id.to_owned(), ::serde_json::json!(self.#name)) });
    }
    let name = &input.ident;
    Ok(quote! {
        impl ::snenk_bot::schema::DescribeValues for #name {
            const OUTPUTS: &'static [::snenk_bot::schema::ConfigOutput] = &[#(#descriptors),*];

            fn values(&self) -> ::snenk_bot::engine::Values {
                ::snenk_bot::engine::Values::from([#(#values),*])
            }
        }
    })
}

fn value_kind(ty: &Type) -> syn::Result<proc_macro2::TokenStream> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new(
            ty.span(),
            "unsupported workflow value type",
        ));
    };
    let Some(segment) = path.path.segments.last() else {
        return Err(syn::Error::new(
            ty.span(),
            "unsupported workflow value type",
        ));
    };
    match segment.ident.to_string().as_str() {
        "String" => Ok(quote!(::snenk_bot::schema::ConfigOutputKind::Text)),
        "Vec" | "Value" | "BTreeMap" => Ok(quote!(::snenk_bot::schema::ConfigOutputKind::Json)),
        "bool" => Ok(quote!(::snenk_bot::schema::ConfigOutputKind::Toggle)),
        "u8" | "u16" | "u32" | "u64" | "i8" | "i16" | "i32" | "i64" | "f32" | "f64" => {
            Ok(quote!(::snenk_bot::schema::ConfigOutputKind::Number))
        }
        _ => Err(syn::Error::new(
            ty.span(),
            "unsupported workflow value type; use String, bool, a number, Vec, Value, BTreeMap, or Option of one",
        )),
    }
}

fn expand_config_schema(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "ConfigSchema does not support generic configuration types",
        ));
    }
    reject_serde_rename_all(&input.attrs)?;

    let mut schema_id = None;
    let mut version = None;
    let mut title = None;
    let mut credential: Option<Path> = None;
    let mut authorization: Option<Path> = None;
    let mut collection: Option<Path> = None;
    let mut actions: Option<Path> = None;
    let mut category: Option<LitStr> = None;
    let mut integration: Option<LitStr> = None;
    let mut outputs = Vec::new();
    let mut output_ids = HashSet::new();

    for attribute in &input.attrs {
        if !attribute.path().is_ident("config") {
            continue;
        }

        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("id") {
                schema_id = Some(meta.value()?.parse::<LitStr>()?);
                Ok(())
            } else if meta.path.is_ident("version") {
                version = Some(meta.value()?.parse::<LitInt>()?);
                Ok(())
            } else if meta.path.is_ident("title") {
                title = Some(meta.value()?.parse::<LitStr>()?);
                Ok(())
            } else if meta.path.is_ident("credential") {
                let path = meta.value()?.parse::<Path>()?;
                if credential.replace(path).is_some() {
                    return Err(meta.error("duplicate credential declaration"));
                }
                Ok(())
            } else if meta.path.is_ident("authorization") {
                let path = meta.value()?.parse::<Path>()?;
                if authorization.replace(path).is_some() {
                    return Err(meta.error("duplicate authorization declaration"));
                }
                Ok(())
            } else if meta.path.is_ident("collection") {
                let path = meta.value()?.parse::<Path>()?;
                if collection.replace(path).is_some() {
                    return Err(meta.error("duplicate collection declaration"));
                }
                Ok(())
            } else if meta.path.is_ident("actions") {
                let path = meta.value()?.parse::<Path>()?;
                if actions.replace(path).is_some() {
                    return Err(meta.error("duplicate actions declaration"));
                }
                Ok(())
            } else if meta.path.is_ident("category") {
                let value = meta.value()?.parse::<LitStr>()?;
                if category.replace(value).is_some() {
                    return Err(meta.error("duplicate category declaration"));
                }
                Ok(())
            } else if meta.path.is_ident("integration") {
                let value = meta.value()?.parse::<LitStr>()?;
                if integration.replace(value).is_some() {
                    return Err(meta.error("duplicate integration declaration"));
                }
                Ok(())
            } else if meta.path.is_ident("output") {
                let content;
                syn::parenthesized!(content in meta.input);
                let args = content.parse_terminated(|input| input.parse::<LitStr>(), Token![,])?;
                if !(3..=5).contains(&args.len()) {
                    return Err(meta.error("output requires id, label, kind, optional description, and optional `optional` marker"));
                }
                let mut args = args.into_iter();
                let id = args.next().expect("output argument count was checked");
                let label = args.next().expect("output argument count was checked");
                let kind = args.next().expect("output argument count was checked");
                let fourth = args.next();
                let fifth = args.next();
                let (description, optional) = match (fourth, fifth) {
                    (None, None) => (LitStr::new("", id.span()), false),
                    (Some(marker), None) if marker.value() == "optional" => (LitStr::new("", id.span()), true),
                    (Some(description), None) => (description, false),
                    (Some(description), Some(marker)) if marker.value() == "optional" => (description, true),
                    (_, Some(marker)) => return Err(syn::Error::new(marker.span(), "last output argument must be `optional`")),
                };
                if id.value().trim().is_empty()
                    || id.value().chars().any(char::is_whitespace)
                {
                    return Err(syn::Error::new(
                        id.span(),
                        "output ID must not be empty or contain whitespace",
                    ));
                }
                if !output_ids.insert(id.value()) {
                    return Err(syn::Error::new(id.span(), "duplicate config output ID"));
                }
                let kind = match kind.value().as_str() {
                    "text" => quote!(::snenk_bot::schema::ConfigOutputKind::Text),
                    "number" => quote!(::snenk_bot::schema::ConfigOutputKind::Number),
                    "toggle" => quote!(::snenk_bot::schema::ConfigOutputKind::Toggle),
                    "json" => quote!(::snenk_bot::schema::ConfigOutputKind::Json),
                    _ => {
                        return Err(syn::Error::new(
                            kind.span(),
                            "output kind must be `text`, `number`, `toggle`, or `json`",
                        ));
                    }
                };
                let output_required = !optional;
                outputs.push(quote! {
                    ::snenk_bot::schema::ConfigOutput {
                        id: #id,
                        label: #label,
                        description: #description,
                        kind: #kind,
                        required: #output_required,
                    }
                });
                Ok(())
            } else {
                Err(meta.error(
                    "supported keys are `id`, `version`, `title`, `credential`, `authorization`, `collection`, `actions`, `category`, `integration`, and repeatable `output(...)`",
                ))
            }
        })?;
    }

    let schema_id = required(
        schema_id,
        input.ident.span(),
        "missing `config(id = \"…\")`",
    )?;
    let version = required(version, input.ident.span(), "missing `config(version = …)`")?;
    let title = required(title, input.ident.span(), "missing `config(title = \"…\")`")?;
    let version_value = version.base10_parse::<u32>()?;
    if version_value == 0 {
        return Err(syn::Error::new(
            version.span(),
            "config version must be at least 1",
        ));
    }
    if category.is_some() != integration.is_some() {
        return Err(syn::Error::new(
            input.ident.span(),
            "category and integration must be declared together",
        ));
    }
    let category = category
        .map(|category| match category.value().as_str() {
            "general" => Ok(quote!(Some(
                ::snenk_bot::schema::ConfigurationCategory::General
            ))),
            "broadcast" => Ok(quote!(Some(
                ::snenk_bot::schema::ConfigurationCategory::Broadcast
            ))),
            "streaming" => Ok(quote!(Some(
                ::snenk_bot::schema::ConfigurationCategory::Streaming
            ))),
            "devices" => Ok(quote!(Some(
                ::snenk_bot::schema::ConfigurationCategory::Devices
            ))),
            _ => Err(syn::Error::new(
                category.span(),
                "unsupported configuration category",
            )),
        })
        .transpose()?
        .unwrap_or_else(|| quote!(None));
    if let Some(id) = &integration
        && (id.value().trim().is_empty() || id.value().chars().any(char::is_whitespace))
    {
        return Err(syn::Error::new(
            id.span(),
            "integration ID must not be empty or contain whitespace",
        ));
    }

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            input.ident.span(),
            "ConfigSchema can only be derived for structs",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new(
            data.fields.span(),
            "ConfigSchema requires named fields",
        ));
    };

    let mut stable_ids = HashSet::new();
    let mut field_descriptors = Vec::with_capacity(fields.named.len());
    let mut preceding_text_fields = HashSet::new();

    for field in &fields.named {
        let mut stable_id = None;
        let mut label = None;
        let mut description = None;
        let mut introduced = None;
        let mut secret = false;
        let mut directory = false;
        let mut choice = None;
        let mut dependency = None;

        for attribute in &field.attrs {
            if !attribute.path().is_ident("config") {
                continue;
            }

            attribute.parse_nested_meta(|meta| {
                if meta.path.is_ident("id") {
                    stable_id = Some(meta.value()?.parse::<LitStr>()?);
                    Ok(())
                } else if meta.path.is_ident("label") {
                    label = Some(meta.value()?.parse::<LitStr>()?);
                    Ok(())
                } else if meta.path.is_ident("description") {
                    description = Some(meta.value()?.parse::<LitStr>()?);
                    Ok(())
                } else if meta.path.is_ident("introduced") {
                    introduced = Some(meta.value()?.parse::<LitInt>()?);
                    Ok(())
                } else if meta.path.is_ident("secret") {
                    secret = true;
                    Ok(())
                } else if meta.path.is_ident("directory") {
                    if directory {
                        return Err(meta.error("duplicate directory declaration"));
                    }
                    directory = true;
                    Ok(())
                } else if meta.path.is_ident("choice") {
                    choice = Some(meta.value()?.parse::<LitStr>()?);
                    Ok(())
                } else if meta.path.is_ident("depends_on") {
                    dependency = Some(meta.value()?.parse::<LitStr>()?);
                    Ok(())
                } else {
                    Err(meta.error(
                        "supported keys are `id`, `label`, `description`, `introduced`, `secret`, `directory`, `choice`, and `depends_on`",
                    ))
                }
            })?;
        }

        let span = field.span();
        let stable_id = required(stable_id, span, "missing field `config(id = \"…\")`")?;
        let label = required(label, span, "missing field `config(label = \"…\")`")?;
        let introduced = required(introduced, span, "missing field `config(introduced = …)`")?;
        let introduced_value = introduced.base10_parse::<u32>()?;
        if introduced_value == 0 || introduced_value > version_value {
            return Err(syn::Error::new(
                introduced.span(),
                format!("field introduction version must be between 1 and {version_value}"),
            ));
        }
        let description = description.unwrap_or_else(|| LitStr::new("", span));

        if !stable_ids.insert(stable_id.value()) {
            return Err(syn::Error::new(
                stable_id.span(),
                format!("duplicate config field id `{}`", stable_id.value()),
            ));
        }

        let serialized_name = serde_field_name(field)?;
        if stable_id.value() != serialized_name {
            return Err(syn::Error::new(
                stable_id.span(),
                format!(
                    "config field id `{}` must match its serialized JSON key `{serialized_name}`; add `#[serde(rename = \"{}\")]` when preserving an ID across a Rust field rename",
                    stable_id.value(),
                    stable_id.value(),
                ),
            ));
        }

        let (value_type, field_required) = optional_inner_type(&field.ty)?;
        let kind = if directory {
            if !is_string(value_type) || secret || choice.is_some() || dependency.is_some() {
                return Err(syn::Error::new(
                    span,
                    "directory is only valid for String or Option<String> fields without secret or choice metadata",
                ));
            }
            quote!(::snenk_bot::schema::ConfigFieldKind::Directory)
        } else {
            field_kind(value_type, secret)?
        };
        if choice.is_some()
            && (secret
                || !is_string(value_type)
                    && !is_string_collection(value_type)
                    && !is_string_set_map(value_type))
        {
            return Err(syn::Error::new(
                span,
                "choice sources are only valid for non-secret String, Vec<String>, BTreeSet<String>, or BTreeMap<String, BTreeSet<String>> fields",
            ));
        }
        let choice_source = if let Some(source) = choice.as_ref() {
            let key = source.value();
            let Some((namespace, resource)) = key.split_once('.') else {
                return Err(syn::Error::new(
                    source.span(),
                    "choice source must use `module.resource`",
                ));
            };
            if namespace.is_empty() || resource.is_empty() || key.chars().any(char::is_whitespace) {
                return Err(syn::Error::new(
                    source.span(),
                    "choice source must use `module.resource` without spaces",
                ));
            }
            let depends_on = if let Some(field_name) = dependency.as_ref() {
                if !preceding_text_fields.contains(&field_name.value()) {
                    return Err(syn::Error::new(
                        field_name.span(),
                        "choice dependency must name a preceding text field",
                    ));
                }
                quote!(Some(#field_name))
            } else {
                quote!(None)
            };
            quote!(Some(::snenk_bot::schema::ConfigChoiceSource { key: #source, depends_on: #depends_on }))
        } else {
            if let Some(field_name) = dependency.as_ref() {
                return Err(syn::Error::new(
                    field_name.span(),
                    "`depends_on` requires a choice source",
                ));
            }
            quote!(None)
        };
        if !secret
            && matches!(value_type, Type::Path(path) if path.path.segments.last().is_some_and(|s| s.ident == "String"))
        {
            preceding_text_fields.insert(stable_id.value());
        }
        field_descriptors.push(quote! {
            ::snenk_bot::schema::ConfigField {
                id: #stable_id,
                label: #label,
                description: #description,
                introduced_in: #introduced_value,
                kind: #kind,
                required: #field_required,
                choice_source: #choice_source,
            }
        });
    }

    let name = &input.ident;
    let credential = credential.map_or_else(|| quote!(None), |path| quote!(Some(#path)));
    let authorization = authorization.map_or_else(|| quote!(None), |path| quote!(Some(#path)));
    let collection = collection.map_or_else(|| quote!(None), |path| quote!(Some(#path)));
    let actions = actions.map_or_else(|| quote!(&[]), |path| quote!(#path));
    let integration = integration.map_or_else(|| quote!(None), |id| quote!(Some(#id)));
    Ok(quote! {
        impl ::snenk_bot::schema::DescribeConfig for #name {
            const SCHEMA: &'static ::snenk_bot::schema::ConfigSchema =
                &::snenk_bot::schema::ConfigSchema {
                    id: #schema_id,
                    version: #version_value,
                    title: #title,
                    fields: &[#(#field_descriptors),*],
                    outputs: &[#(#outputs),*],
                };
            const CREDENTIAL: Option<::snenk_bot::schema::CredentialDescriptor> = #credential;
            const AUTHORIZATION: Option<::snenk_bot::schema::AuthorizationDescriptor> = #authorization;
            const COLLECTION: Option<::snenk_bot::schema::ConfigurationCollectionDescriptor> = #collection;
            const ACTIONS: &'static [::snenk_bot::schema::ConfigurationActionDescriptor] = #actions;
            const CATEGORY: Option<::snenk_bot::schema::ConfigurationCategory> = #category;
            const INTEGRATION: Option<&'static str> = #integration;
        }
    })
}

fn reject_serde_rename_all(attributes: &[syn::Attribute]) -> syn::Result<()> {
    for attribute in attributes {
        if !attribute.path().is_ident("serde") {
            continue;
        }
        let Meta::List(list) = &attribute.meta else {
            continue;
        };
        let entries = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
        if let Some(entry) = entries
            .iter()
            .find(|entry| entry.path().is_ident("rename_all"))
        {
            return Err(syn::Error::new(
                entry.span(),
                "ConfigSchema does not support `serde(rename_all)` because stable field IDs must match stored JSON keys",
            ));
        }
    }
    Ok(())
}

fn serde_field_name(field: &syn::Field) -> syn::Result<String> {
    let mut name = field
        .ident
        .as_ref()
        .expect("named fields were checked before parsing field metadata")
        .to_string();

    for attribute in &field.attrs {
        if !attribute.path().is_ident("serde") {
            continue;
        }
        let Meta::List(list) = &attribute.meta else {
            continue;
        };
        let entries = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
        for entry in entries {
            if !entry.path().is_ident("rename") {
                continue;
            }
            let Meta::NameValue(rename) = entry else {
                return Err(syn::Error::new(
                    entry.span(),
                    "ConfigSchema requires one shared Serde name; use `serde(rename = \"…\")`",
                ));
            };
            let Expr::Lit(expression) = rename.value else {
                return Err(syn::Error::new(
                    rename.span(),
                    "Serde field rename must be a string",
                ));
            };
            let Lit::Str(value) = expression.lit else {
                return Err(syn::Error::new(
                    expression.span(),
                    "Serde field rename must be a string",
                ));
            };
            name = value.value();
        }
    }

    Ok(name)
}

fn required<T>(value: Option<T>, span: proc_macro2::Span, message: &str) -> syn::Result<T> {
    value.ok_or_else(|| syn::Error::new(span, message))
}

fn optional_inner_type(ty: &Type) -> syn::Result<(&Type, bool)> {
    let Type::Path(path) = ty else {
        return Ok((ty, true));
    };
    let Some(segment) = path.path.segments.last() else {
        return Ok((ty, true));
    };
    if segment.ident != "Option" {
        return Ok((ty, true));
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new(
            ty.span(),
            "Option must have one supported value type",
        ));
    };
    if arguments.args.len() != 1 {
        return Err(syn::Error::new(
            ty.span(),
            "Option must have one supported value type",
        ));
    }
    let Some(syn::GenericArgument::Type(inner)) = arguments.args.first() else {
        return Err(syn::Error::new(
            ty.span(),
            "Option must have one supported value type",
        ));
    };
    Ok((inner, false))
}

fn field_kind(ty: &Type, secret: bool) -> syn::Result<proc_macro2::TokenStream> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new(
            ty.span(),
            "unsupported configuration field type",
        ));
    };
    let Some(segment) = path.path.segments.last() else {
        return Err(syn::Error::new(
            ty.span(),
            "unsupported configuration field type",
        ));
    };
    let type_name = segment.ident.to_string();

    match (type_name.as_str(), secret) {
        ("Value", false)
            if path.path.segments.len() == 2
                && path
                    .path
                    .segments
                    .first()
                    .is_some_and(|part| part.ident == "serde_json") =>
        {
            Ok(quote!(::snenk_bot::schema::ConfigFieldKind::Json))
        }
        ("String", false) => Ok(quote!(::snenk_bot::schema::ConfigFieldKind::Text)),
        ("String", true) => Ok(quote!(::snenk_bot::schema::ConfigFieldKind::Secret)),
        ("bool", false) => Ok(quote!(::snenk_bot::schema::ConfigFieldKind::Toggle)),
        ("u8" | "u16" | "u32" | "u64" | "i8" | "i16" | "i32" | "i64", false) => {
            Ok(quote!(::snenk_bot::schema::ConfigFieldKind::Integer))
        }
        ("Vec", false) if generic_types(segment, 1).is_some_and(|args| is_string(args[0])) => {
            Ok(quote!(::snenk_bot::schema::ConfigFieldKind::StringList))
        }
        ("BTreeSet", false) if generic_types(segment, 1).is_some_and(|args| is_string(args[0])) => {
            Ok(quote!(::snenk_bot::schema::ConfigFieldKind::StringSet))
        }
        ("BTreeMap", false)
            if generic_types(segment, 2)
                .is_some_and(|args| is_string(args[0]) && is_string_set(args[1])) =>
        {
            Ok(quote!(::snenk_bot::schema::ConfigFieldKind::StringSetMap))
        }
        (_, true) => Err(syn::Error::new(
            ty.span(),
            "`secret` is only valid for String fields",
        )),
        (_, false)
            if matches!(segment.arguments, syn::PathArguments::None)
                && type_name.chars().next().is_some_and(char::is_uppercase) =>
        {
            Ok(quote!(::snenk_bot::schema::ConfigFieldKind::Object(
                <#ty as ::snenk_bot::schema::DescribeConfig>::SCHEMA.fields
            )))
        }
        _ => Err(syn::Error::new(
            ty.span(),
            "unsupported configuration field type; supported types are String, bool, integers, Vec<String>, BTreeSet<String>, BTreeMap<String, BTreeSet<String>>, and structs deriving ConfigSchema",
        )),
    }
}

/// Returns the exact generic type arguments of a collection segment.
fn generic_types(segment: &syn::PathSegment, expected: usize) -> Option<Vec<&Type>> {
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    let types = arguments
        .args
        .iter()
        .map(|argument| match argument {
            syn::GenericArgument::Type(ty) => Some(ty),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    (types.len() == expected).then_some(types)
}

/// Checks whether a type is a plain string value.
fn is_string(ty: &Type) -> bool {
    matches!(ty, Type::Path(path) if path.path.segments.last().is_some_and(|segment| {
        segment.ident == "String" && matches!(segment.arguments, syn::PathArguments::None)
    }))
}

/// Checks whether a type is a set of strings.
fn is_string_set(ty: &Type) -> bool {
    matches!(ty, Type::Path(path) if path.path.segments.last().is_some_and(|segment| {
        segment.ident == "BTreeSet" && generic_types(segment, 1).is_some_and(|args| is_string(args[0]))
    }))
}

/// Checks whether a type maps string keys to sets of strings.
fn is_string_set_map(ty: &Type) -> bool {
    matches!(ty, Type::Path(path) if path.path.segments.last().is_some_and(|segment| {
        segment.ident == "BTreeMap" && generic_types(segment, 2).is_some_and(|args| {
            is_string(args[0]) && is_string_set(args[1])
        })
    }))
}

/// Checks whether a type is a list or set of strings.
fn is_string_collection(ty: &Type) -> bool {
    is_string_set(ty)
        || matches!(ty, Type::Path(path) if path.path.segments.last().is_some_and(|segment| {
            segment.ident == "Vec" && generic_types(segment, 1).is_some_and(|args| is_string(args[0]))
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_values_emit_typed_public_fields_and_skip_secrets() {
        let input: DeriveInput = syn::parse2(quote! {
            #[workflow_values(event = "sample.event")]
            struct Sample {
                #[value(label = "Message")]
                message: String,
                #[value(label = "Count")]
                count: Option<u64>,
                #[value(secret)]
                token: String,
            }
        })
        .unwrap();
        let expanded = expand_workflow_values(input).unwrap().to_string();
        assert!(expanded.contains("ConfigOutputKind :: Text"));
        assert!(expanded.contains("ConfigOutputKind :: Number"));
        assert!(expanded.contains("required : false"));
        assert!(expanded.contains("sample.event"));
        assert!(!expanded.contains("token"));
    }

    /// Checks structured output metadata from both supported derive annotations.
    #[test]
    fn structured_outputs_keep_their_json_shape_in_metadata() {
        let input: DeriveInput = syn::parse2(quote! {
            struct Results { #[value(label = "Matches")] matches: Vec<String> }
        })
        .unwrap();
        assert!(
            expand_workflow_values(input)
                .unwrap()
                .to_string()
                .contains("ConfigOutputKind :: Json")
        );
        let input: DeriveInput = syn::parse2(quote! {
            #[config(id = "lookup", version = 1, title = "Lookup", output("results", "Results", "json"))]
            struct Lookup {}
        }).unwrap();
        assert!(
            expand_config_schema(input)
                .unwrap()
                .to_string()
                .contains("ConfigOutputKind :: Json")
        );
    }

    #[test]
    fn workflow_values_reject_missing_labels_and_unsupported_types() {
        for field in [
            quote!(message: String,),
            quote!(#[value(label = "Items")] items: (String, String),),
            quote!(#[value(label = "Token", secret)] token: String,),
        ] {
            let input: DeriveInput = syn::parse2(quote! {
                struct Sample { #field }
            })
            .unwrap();
            assert!(expand_workflow_values(input).is_err());
        }
    }

    #[test]
    fn rejects_unsupported_optional_shapes() {
        for field_type in [
            quote!(Option<Option<String>>),
            quote!(Option<Vec<u32>>),
            quote!(Option<(String, String)>),
            quote!(BTreeMap<String, Vec<String>>),
            quote!(BTreeMap<u32, BTreeSet<String>>),
        ] {
            let input: DeriveInput = syn::parse2(quote! {
                #[config(id = "test", version = 1, title = "Test")]
                struct Config {
                    #[config(id = "value", label = "Value", introduced = 1)]
                    value: #field_type,
                }
            })
            .unwrap();
            assert!(expand_config_schema(input).is_err());
        }
    }

    /// Directory controls preserve string optionality and emit integration-owned action metadata.
    #[test]
    fn derives_directory_fields_and_record_actions() {
        let input: DeriveInput = syn::parse2(quote! {
            #[config(id = "test", version = 1, title = "Test", actions = RECORD_ACTIONS)]
            struct Config {
                #[config(id = "root", label = "Root", introduced = 1, directory)]
                root: String,
                #[config(id = "optional", label = "Optional", introduced = 1, directory)]
                optional: Option<String>,
            }
        })
        .unwrap();
        let expanded = expand_config_schema(input).unwrap().to_string();
        assert_eq!(expanded.matches("ConfigFieldKind :: Directory").count(), 2);
        assert!(expanded.contains("required : false"));
        assert!(expanded.contains("ConfigurationActionDescriptor"));
        assert!(expanded.contains("= RECORD_ACTIONS"));
    }

    /// Directory metadata cannot bypass string, secret, or choice type checks.
    #[test]
    fn rejects_invalid_directory_metadata_and_duplicate_actions() {
        for (ty, attributes) in [
            (quote!(bool), quote!(directory)),
            (quote!(Vec<String>), quote!(directory)),
            (quote!(String), quote!(directory, secret)),
            (quote!(String), quote!(directory, choice = "test.folders")),
            (quote!(String), quote!(directory, directory)),
        ] {
            let input: DeriveInput = syn::parse2(quote! {
                #[config(id = "test", version = 1, title = "Test")]
                struct Config { #[config(id = "root", label = "Root", introduced = 1, #attributes)] root: #ty }
            }).unwrap();
            assert!(expand_config_schema(input).is_err());
        }
        let input: DeriveInput = syn::parse2(quote! {
            #[config(id = "test", version = 1, title = "Test", actions = FIRST, actions = SECOND)]
            struct Config {}
        })
        .unwrap();
        assert!(expand_config_schema(input).is_err());
    }

    /// Nested settings and supported collection types produce reusable field declarations.
    #[test]
    fn derives_nested_objects_and_string_collections() {
        let input: DeriveInput = syn::parse2(quote! {
            #[config(id = "test", version = 1, title = "Test")]
            struct Config {
                #[config(id = "nested", label = "Nested", introduced = 1)]
                nested: Nested,
                #[config(id = "names", label = "Names", introduced = 1)]
                names: Vec<String>,
                #[config(id = "channels", label = "Channels", introduced = 1, choice = "api.channels")]
                channels: BTreeSet<String>,
                #[config(id = "providers", label = "Providers", introduced = 1, choice = "api.provider_operations")]
                providers: BTreeMap<String, BTreeSet<String>>,
                #[config(id = "payload", label = "Payload", introduced = 1)]
                payload: serde_json::Value,
            }
        })
        .unwrap();
        let expanded = expand_config_schema(input).unwrap().to_string();
        assert!(expanded.contains("ConfigFieldKind :: Object"));
        assert!(expanded.contains("Nested as :: snenk_bot :: schema :: DescribeConfig"));
        assert!(expanded.contains("ConfigFieldKind :: StringList"));
        assert!(expanded.contains("ConfigFieldKind :: StringSet"));
        assert!(expanded.contains("ConfigFieldKind :: StringSetMap"));
        assert!(expanded.contains("ConfigFieldKind :: Json"));
        assert!(expanded.contains("api.provider_operations"));
    }

    /// Config-owned controls are emitted as associated metadata on the existing schema trait.
    #[test]
    fn derives_optional_config_controls_and_rejects_duplicates() {
        let input: DeriveInput = syn::parse2(quote! {
            #[config(id = "test", version = 1, title = "Test",
                credential = PASSWORD, authorization = PAIRING, collection = RECORDS)]
            struct Config {
                #[config(id = "enabled", label = "Enabled", introduced = 1)]
                enabled: bool,
            }
        })
        .unwrap();
        let expanded = expand_config_schema(input).unwrap().to_string();
        for expected in ["Some (PASSWORD)", "Some (PAIRING)", "Some (RECORDS)"] {
            assert!(expanded.contains(expected), "missing {expected}");
        }
        for duplicate in [
            quote!(credential = PASSWORD, credential = OTHER),
            quote!(authorization = PAIRING, authorization = OTHER),
            quote!(collection = RECORDS, collection = OTHER),
        ] {
            let input: DeriveInput = syn::parse2(quote! {
                #[config(id = "test", version = 1, title = "Test", #duplicate)]
                struct Config {
                    #[config(id = "enabled", label = "Enabled", introduced = 1)]
                    enabled: bool,
                }
            })
            .unwrap();
            assert!(expand_config_schema(input).is_err());
        }
    }

    /// Directory identity is paired and its finite categories are checked at compile time.
    #[test]
    fn validates_config_directory_metadata() {
        let valid: DeriveInput = syn::parse2(quote! {
            #[config(id = "test", version = 1, title = "Test",
                category = "devices", integration = "test-device")]
            struct Config { #[config(id = "enabled", label = "Enabled", introduced = 1)] enabled: bool }
        }).unwrap();
        let expanded = expand_config_schema(valid).unwrap().to_string();
        assert!(expanded.contains("ConfigurationCategory :: Devices"));
        assert!(expanded.contains("Some (\"test-device\")"));
        for invalid in [
            quote!(category = "devices"),
            quote!(integration = "test-device"),
            quote!(category = "unknown", integration = "test-device"),
            quote!(category = "devices", integration = ""),
            quote!(
                category = "devices",
                integration = "test",
                integration = "other"
            ),
            quote!(
                category = "devices",
                category = "general",
                integration = "test"
            ),
        ] {
            let input: DeriveInput = syn::parse2(quote! {
                #[config(id = "test", version = 1, title = "Test", #invalid)]
                struct Config { #[config(id = "enabled", label = "Enabled", introduced = 1)] enabled: bool }
            }).unwrap();
            assert!(expand_config_schema(input).is_err());
        }
    }

    #[test]
    fn emits_output_and_dependent_choice_metadata() {
        let input: DeriveInput = syn::parse2(quote! {
            #[config(id = "test", version = 1, title = "Test",
                output("result", "Result", "text", "optional"))]
            struct Config {
                #[config(id = "scene", label = "Scene", introduced = 1, choice = "obs.scenes")]
                scene: String,
                #[config(id = "item", label = "Item", introduced = 1,
                    choice = "obs.scene_items", depends_on = "scene")]
                item: String,
            }
        })
        .unwrap();
        let expanded = expand_config_schema(input).unwrap().to_string();
        assert!(expanded.contains("ConfigOutputKind :: Text"));
        assert!(expanded.contains("required : false"));
        assert!(expanded.contains("ConfigChoiceSource { key : \"obs.scenes\""));
        assert!(expanded.contains("depends_on : Some (\"scene\")"));
    }

    #[test]
    fn rejects_invalid_choice_dependencies_and_output_ids() {
        for source in [
            quote! { #[config(id = "test", version = 1, title = "Test")] struct C {
                #[config(id = "scene", label = "Scene", introduced = 1)] scene: String,
                #[config(id = "item", label = "Item", introduced = 1, choice = "obs.scene_items", depends_on = "missing")] item: String,
            } },
            quote! { #[config(id = "test", version = 1, title = "Test")] struct C {
                #[config(id = "item", label = "Item", introduced = 1, choice = "obs.hotkeys", depends_on = "x")] item: String,
            } },
            quote! { #[config(id = "test", version = 1, title = "Test")] struct C {
                #[config(id = "item", label = "Item", introduced = 1, choice = "unknown")] item: String,
            } },
            quote! { #[config(id = "test", version = 1, title = "Test", output("x", "X", "date"))] struct C {} },
            quote! { #[config(id = "test", version = 1, title = "Test", output("x", "X", "text"), output("x", "Y", "text"))] struct C {} },
        ] {
            let input: DeriveInput = syn::parse2(source).unwrap();
            assert!(expand_config_schema(input).is_err());
        }
    }
}
