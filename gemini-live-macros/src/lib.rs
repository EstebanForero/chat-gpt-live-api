extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{ToTokens, format_ident, quote};
use syn::{
    AngleBracketedGenericArguments, Attribute, Error, FnArg, GenericArgument, Ident, ItemFn, Lit,
    LitStr, Meta, MetaNameValue, Pat, PatType, Path, PathArguments, PathSegment, ReturnType, Type,
    TypePath, Visibility, parse_macro_input, punctuated::Punctuated, token,
};

fn get_description_from_attrs(attrs: &[Attribute], error_span: Span) -> Result<String, Error> {
    let mut doc_lines = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("doc") {
            if let Meta::NameValue(MetaNameValue {
                value: syn::Expr::Lit(expr_lit),
                ..
            }) = &attr.meta
            {
                if let Lit::Str(lit_str) = &expr_lit.lit {
                    doc_lines.push(lit_str.value().trim_start().to_string());
                } else {
                    return Err(Error::new_spanned(
                        &expr_lit.lit,
                        "Expected string literal in doc attribute",
                    ));
                }
            } else {
                return Err(Error::new_spanned(
                    attr,
                    "Unsupported doc attribute format. Use /// comment or #[doc = \"...\"]",
                ));
            }
        }
    }
    if doc_lines.is_empty() {
        Err(Error::new(
            error_span,
            "Missing description: Add a /// doc comment above the function.",
        ))
    } else {
        Ok(doc_lines.join("\n").trim().to_string())
    }
}

fn rust_type_to_schema_info(ty: &Type) -> Option<(String, bool)> {
    if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            let type_name = segment.ident.to_string();
            if type_name == "Option" {
                if let PathArguments::AngleBracketed(angle_args) = &segment.arguments {
                    if let Some(GenericArgument::Type(inner_ty)) = angle_args.args.first() {
                        return rust_type_to_schema_info(inner_ty).map(|(s, _)| (s, true));
                    }
                }
            }
            match type_name.as_str() {
                "String" => Some(("STRING".to_string(), false)),
                "i32" | "i64" | "u32" | "u64" | "isize" | "usize" => {
                    Some(("INTEGER".to_string(), false))
                }
                "f32" | "f64" => Some(("NUMBER".to_string(), false)),
                "bool" => Some(("BOOLEAN".to_string(), false)),
                _ => None,
            }
        } else {
            None
        }
    } else {
        None
    }
}

fn get_result_types(ty: &Type) -> Option<(&Type, &Type)> {
    if let Type::Path(TypePath {
        path: Path { segments, .. },
        ..
    }) = ty
    {
        if let Some(PathSegment {
            ident,
            arguments: PathArguments::AngleBracketed(AngleBracketedGenericArguments { args, .. }),
        }) = segments.last()
        {
            if ident == "Result" && args.len() == 2 {
                if let (Some(GenericArgument::Type(ok_ty)), Some(GenericArgument::Type(err_ty))) =
                    (args.first(), args.last())
                {
                    return Some((ok_ty, err_ty));
                }
            }
        }
    }
    None
}

fn is_arc_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if let Some(last_seg) = path.segments.last() {
            if last_seg.ident == "Arc" {
                if matches!(last_seg.arguments, PathArguments::AngleBracketed(_)) {
                    if path.segments.len() == 1 {
                        return true;
                    }
                    if path.segments.len() >= 3 {
                        if path.segments[path.segments.len() - 3].ident == "std"
                            && path.segments[path.segments.len() - 2].ident == "sync"
                        {
                            return true;
                        }
                    }
                    if path.leading_colon.is_some()
                        && path.segments.len() >= 3
                        && path.segments[0].ident == "std"
                        && path.segments[1].ident == "sync"
                        && path.segments[2].ident == "Arc"
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[proc_macro_attribute]
pub fn tool_function(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let func_vis = &func.vis;
    let func_sig = &func.sig;
    let func_name = &func_sig.ident;
    let func_async = func_sig.asyncness.is_some();

    if !func_async {
        return TokenStream::from(
            Error::new_spanned(func_sig.fn_token, "Tool function must be async").to_compile_error(),
        );
    }

    let description_lit = parse_macro_input!(attr as LitStr);
    let description = description_lit.value();

    let mut json_param_idents_for_schema_count = Vec::<Ident>::new();
    let mut required_params_for_schema = Vec::<proc_macro2::TokenStream>::new();
    let mut schema_properties = quote! {};
    let mut adapter_arg_parsing_code = quote! {};

    let mut original_fn_invocation_args = Vec::<proc_macro2::TokenStream>::new();
    let closure_state_param_name = format_ident!("__tool_handler_actual_client_state_arg");

    let mut input_iter = func_sig.inputs.iter();

    let mut concrete_state_type_for_closure: Option<Type> = None;

    if let Some(FnArg::Typed(PatType { ty, .. })) = func_sig.inputs.first() {
        if is_arc_type(ty) {
            input_iter.next();
            original_fn_invocation_args.push(quote! { #closure_state_param_name.clone() });

            if let Type::Path(type_path) = &**ty {
                if let Some(segment) = type_path.path.segments.last() {
                    if segment.ident == "Arc" {
                        if let PathArguments::AngleBracketed(angle_args) = &segment.arguments {
                            if let Some(GenericArgument::Type(inner_ty)) = angle_args.args.first() {
                                concrete_state_type_for_closure = Some(inner_ty.clone());
                            } else {
                                return TokenStream::from(Error::new_spanned(ty, "Arc state argument must have a generic type parameter, e.g., Arc<MyState>").to_compile_error());
                            }
                        } else {
                            return TokenStream::from(Error::new_spanned(ty, "Arc state argument must be angle bracketed, e.g., Arc<MyState>").to_compile_error());
                        }
                    }
                }
            }
            if concrete_state_type_for_closure.is_none() {
                return TokenStream::from(
                    Error::new_spanned(ty, "Could not extract inner type from Arc state argument.")
                        .to_compile_error(),
                );
            }
        }
    }

    for input in input_iter {
        if let FnArg::Typed(PatType { pat, ty, .. }) = input {
            if let Pat::Ident(pat_ident) = &**pat {
                let param_name_ident = &pat_ident.ident;
                let param_name_str = param_name_ident.to_string();

                json_param_idents_for_schema_count.push(param_name_ident.clone());
                original_fn_invocation_args.push(quote! { #param_name_ident });

                let (schema_type_str, is_optional) = match rust_type_to_schema_info(ty) {
                    Some(info) => info,
                    None => {
                        return TokenStream::from(
                            Error::new_spanned(
                                ty,
                                format!(
                                    "Unsupported parameter type for schema: {}",
                                    ty.to_token_stream()
                                ),
                            )
                            .to_compile_error(),
                        );
                    }
                };
                schema_properties.extend(quote! { (#param_name_str.to_string(), ::gemini_live_api::types::Schema { schema_type: #schema_type_str.to_string(), ..Default::default() }, ), });
                if !is_optional {
                    required_params_for_schema.push(quote! { #param_name_str.to_string() });
                }
                let temp_val_ident = format_ident!("__tool_arg_{}_json_val", param_name_str);
                let parsing_code = if is_optional {
                    quote! {
                        let #temp_val_ident = args_obj.get(#param_name_str).cloned();
                        let #param_name_ident : #ty = match #temp_val_ident {
                             Some(val) => Some(::serde_json::from_value(val).map_err(|e| format!("Failed to parse optional argument '{}': {}", #param_name_str, e))?),
                             None => None,
                        };
                    }
                } else {
                    quote! {
                        let #temp_val_ident = args_obj.get(#param_name_str).cloned();
                        let #param_name_ident : #ty = match #temp_val_ident {
                             Some(val) => ::serde_json::from_value(val).map_err(|e| format!("Failed to parse required argument '{}': {}", #param_name_str, e))?,
                             None => return Err(format!("Missing required argument '{}'", #param_name_str)),
                        };
                    }
                };
                adapter_arg_parsing_code.extend(parsing_code);
            } else {
                return TokenStream::from(
                    Error::new_spanned(pat, "Parameter must be a simple ident").to_compile_error(),
                );
            }
        } else {
            return TokenStream::from(
                Error::new_spanned(input, "Unsupported argument type (expected typed ident)")
                    .to_compile_error(),
            );
        }
    }

    let number_of_json_params = json_param_idents_for_schema_count.len();
    let func_name_str = func_name.to_string();
    let schema_map = if schema_properties.is_empty() {
        quote! { None }
    } else {
        quote! { Some(::std::collections::HashMap::from([#schema_properties])) }
    };
    let required_vec = if required_params_for_schema.is_empty() {
        quote! { None }
    } else {
        quote! { Some(vec![#(#required_params_for_schema),*]) }
    };
    let parameters_schema = quote! { Some(::gemini_live_api::types::Schema { schema_type: "OBJECT".to_string(), properties: #schema_map, required: #required_vec, ..Default::default() }) };

    let declaration_static_name =
        format_ident!("{}_DECLARATION", func_name.to_string().to_uppercase());
    let get_declaration_fn_name = format_ident!("{}_get_declaration", func_name);
    let declaration_code = quote! {
         #func_vis static #declaration_static_name: ::std::sync::OnceLock<::gemini_live_api::types::FunctionDeclaration> = ::std::sync::OnceLock::new();
         #[doc(hidden)]
         #func_vis fn #get_declaration_fn_name() -> &'static ::gemini_live_api::types::FunctionDeclaration {
              #declaration_static_name.get_or_init(|| {
                   ::gemini_live_api::types::FunctionDeclaration {
                        name: #func_name_str.to_string(),
                        description: #description.to_string(),
                        parameters: #parameters_schema,
                   }
              })
         }
    };

    let original_fn_invocation_code_block =
        quote! { #func_name(#(#original_fn_invocation_args),*) };

    let adapter_return_handling_code = match &func_sig.output {
        ReturnType::Default => {
            quote! { #original_fn_invocation_code_block.await; Ok(::serde_json::json!({})) }
        }
        ReturnType::Type(_, ty) => {
            if let Some((_ok_ty, _err_ty)) = get_result_types(ty) {
                quote! {
                    let result_val = #original_fn_invocation_code_block.await;
                    match result_val {
                        Ok(ok_val) => ::serde_json::to_value(ok_val).map(|v| ::serde_json::json!({ "result": v })).map_err(|e| format!("Failed to serialize success result: {}", e)),
                        Err(err_val) => Ok(::serde_json::json!({ "error": format!("{}", err_val) })),
                    }
                }
            } else {
                quote! {
                    let result_val = #original_fn_invocation_code_block.await;
                    ::serde_json::to_value(result_val).map(|v| ::serde_json::json!({ "result": v })).map_err(|e| format!("Failed to serialize function result type {}: {}", stringify!(#ty), e))
                }
            }
        }
    };

    let register_tool_fn_name = format_ident!("{}_register_tool", func_name);

    let registration_code = if let Some(concrete_state_ty) = concrete_state_type_for_closure {
        quote! {
             #[doc(hidden)]
             #func_vis fn #register_tool_fn_name(
                mut builder: ::gemini_live_api::client::builder::GeminiLiveClientBuilder<#concrete_state_ty>
             ) -> ::gemini_live_api::client::builder::GeminiLiveClientBuilder<#concrete_state_ty>
             {
                let tool_adapter_closure = move |args: Option<::serde_json::Value>, #closure_state_param_name: ::std::sync::Arc<#concrete_state_ty>| async move {
                    let args_obj = match args {
                         Some(::serde_json::Value::Object(map)) => map,
                         Some(v) => return Err(format!("Tool arguments must be a JSON object, got: {:?}", v)),
                         None if #number_of_json_params > 0 => return Err("Tool requires arguments, but none were provided.".to_string()),
                         None => ::serde_json::Map::new(),
                    };

                    #adapter_arg_parsing_code

                    #adapter_return_handling_code
                };

                 builder = builder.add_tool_declaration(#get_declaration_fn_name().clone());
                 builder = builder.on_tool_call(#func_name_str, tool_adapter_closure);
                 builder
             }
        }
    } else {
        quote! {
             #[doc(hidden)]
             #func_vis fn #register_tool_fn_name<S_CLIENT_BUILDER_STATE>(
                mut builder: ::gemini_live_api::client::builder::GeminiLiveClientBuilder<S_CLIENT_BUILDER_STATE>
             ) -> ::gemini_live_api::client::builder::GeminiLiveClientBuilder<S_CLIENT_BUILDER_STATE>
             where S_CLIENT_BUILDER_STATE: Clone + Send + Sync + 'static
             {
                let tool_adapter_closure = move |args: Option<::serde_json::Value>, _ignored_state: ::std::sync::Arc<S_CLIENT_BUILDER_STATE>| async move {
                    let args_obj = match args {
                         Some(::serde_json::Value::Object(map)) => map,
                         Some(v) => return Err(format!("Tool arguments must be a JSON object, got: {:?}", v)),
                         None if #number_of_json_params > 0 => return Err("Tool requires arguments, but none were provided.".to_string()),
                         None => ::serde_json::Map::new(),
                    };

                    #adapter_arg_parsing_code
                    #adapter_return_handling_code
                };
                 builder = builder.add_tool_declaration(#get_declaration_fn_name().clone());
                 builder = builder.on_tool_call(#func_name_str, tool_adapter_closure);
                 builder
             }
        }
    };

    let output = quote! {
        #func
        #declaration_code
        #registration_code
    };
    output.into()
}
