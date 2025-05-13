extern crate proc_macro;

use proc_macro::TokenStream;
use quote::{ToTokens, format_ident, quote};
use serde_json::json;
use syn::{
    AngleBracketedGenericArguments, Attribute, Error, FnArg, GenericArgument, ItemFn, Lit, LitStr,
    Meta, MetaNameValue, Pat, PatType, Path, PathArguments, PathSegment, ReturnType, Type,
    TypePath, parse_macro_input,
};

// Helper to extract the inner type from Arc<T>
fn get_arc_inner_type(ty: &Type) -> Option<&Type> {
    if let Type::Path(TypePath { path, .. }) = ty {
        if let Some(last_seg) = path.segments.last() {
            if last_seg.ident == "Arc" {
                if let PathArguments::AngleBracketed(AngleBracketedGenericArguments {
                    args, ..
                }) = &last_seg.arguments
                {
                    if let Some(GenericArgument::Type(inner_ty)) = args.first() {
                        let is_std_arc = path.segments.len() == 1
                            || (path.segments.len() >= 3
                                && path.segments[path.segments.len() - 3].ident == "std"
                                && path.segments[path.segments.len() - 2].ident == "sync")
                            || (path.leading_colon.is_some()
                                && path.segments.len() >= 3
                                && path.segments[0].ident == "std"
                                && path.segments[1].ident == "sync");
                        if is_std_arc {
                            return Some(inner_ty);
                        }
                    }
                }
            }
        }
    }
    None
}

fn get_description_from_doc_attrs(attrs: &[Attribute]) -> Option<String> {
    let mut doc_lines = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("doc") {
            if let Meta::NameValue(MetaNameValue {
                value: syn::Expr::Lit(expr_lit),
                ..
            }) = &attr.meta
            {
                if let Lit::Str(lit_str) = &expr_lit.lit {
                    doc_lines.push(lit_str.value().trim().to_string());
                }
            }
        }
    }
    if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join("\n"))
    }
}

fn rust_type_to_schema_info(ty: &Type) -> Result<(String, bool /* is_optional */), Error> {
    if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            let type_name = segment.ident.to_string();
            if type_name == "Option" {
                if let PathArguments::AngleBracketed(angle_args) = &segment.arguments {
                    if let Some(GenericArgument::Type(inner_ty)) = angle_args.args.first() {
                        return rust_type_to_schema_info(inner_ty).map(|(s, _)| (s, true));
                    }
                }
                return Err(Error::new_spanned(
                    ty,
                    "Option type must have a generic argument (e.g., Option<String>).",
                ));
            }
            let schema_type_str = match type_name.as_str() {
                "String" => "STRING",
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "isize" | "usize" => {
                    "INTEGER"
                }
                "f32" | "f64" => "NUMBER",
                "bool" => "BOOLEAN",
                _ => {
                    return Err(Error::new_spanned(
                        ty,
                        format!(
                            "Unsupported parameter type for Gemini/OpenAI schema: {}. Supported types: String, iXX/uXX, fXX, bool, Option<T>.",
                            ty.to_token_stream()
                        ),
                    ));
                }
            };
            return Ok((schema_type_str.to_string(), false));
        }
    }
    Err(Error::new_spanned(
        ty,
        "Parameter type must be a simple path type.",
    ))
}

fn rust_type_to_openai_json_schema_type(ty: &Type) -> Result<(String, bool), Error> {
    if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            let type_name = segment.ident.to_string();
            if type_name == "Option" {
                if let PathArguments::AngleBracketed(angle_args) = &segment.arguments {
                    if let Some(GenericArgument::Type(inner_ty)) = angle_args.args.first() {
                        return rust_type_to_openai_json_schema_type(inner_ty)
                            .map(|(s, _)| (s, true));
                    }
                }
                return Err(Error::new_spanned(
                    ty,
                    "Option type must have a generic argument (e.g., Option<String>).",
                ));
            }
            let schema_type_str = match type_name.as_str() {
                "String" => "string",
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "isize" | "usize" => {
                    "integer"
                }
                "f32" | "f64" => "number",
                "bool" => "boolean",
                _ => {
                    return Err(Error::new_spanned(
                        ty,
                        format!(
                            "Unsupported parameter type for OpenAI schema: {}. Supported types: String, iXX/uXX, fXX, bool, Option<T>.",
                            ty.to_token_stream()
                        ),
                    ));
                }
            };
            return Ok((schema_type_str.to_string(), false));
        }
    }
    Err(Error::new_spanned(
        ty,
        "Parameter type must be a simple path type for OpenAI schema.",
    ))
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
            if (ident == "Result")
                || (segments.len() > 1
                    && segments[segments.len() - 2].ident == "result"
                    && segments.last().unwrap().ident == "Result")
            {
                if args.len() == 2 {
                    if let (
                        Some(GenericArgument::Type(ok_ty)),
                        Some(GenericArgument::Type(err_ty)),
                    ) = (args.first(), args.last())
                    {
                        return Some((ok_ty, err_ty));
                    }
                }
            }
        }
    }
    None
}

#[proc_macro_attribute]
pub fn tool_function(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let func_sig = &func.sig;
    let func_name = &func_sig.ident;
    let func_async = func_sig.asyncness.is_some();
    let func_name_str = func_name.to_string();

    let description_from_attr = if !attr.is_empty() {
        match parse_macro_input!(attr as LitStr) {
            lit_str => Some(lit_str.value()),
        }
    } else {
        None
    };

    let description_from_docs = get_description_from_doc_attrs(&func.attrs);

    let final_description = match (description_from_attr, description_from_docs) {
        (Some(attr_desc), _) => attr_desc,
        (None, Some(doc_desc)) => doc_desc,
        (None, None) => {
            return TokenStream::from(
                Error::new(
                    func_name.span(),
                    "Missing tool description: Add a /// doc comment or provide it in the attribute, e.g., #[tool_function(\"My description\")]",
                )
                .to_compile_error(),
            );
        }
    };

    if !func_async {
        return TokenStream::from(
            Error::new_spanned(func_sig.fn_token, "Tool function must be async").to_compile_error(),
        );
    }

    let mut gemini_schema_properties = quote! {};
    let mut gemini_required_params = Vec::<proc_macro2::TokenStream>::new();
    let mut openai_schema_properties_map = serde_json::Map::new();
    let mut openai_required_params = Vec::new();
    let mut adapter_arg_parsing_code = quote! {};
    let mut original_fn_invocation_args = Vec::<proc_macro2::TokenStream>::new();

    let mut input_iter = func_sig.inputs.iter();
    let mut concrete_state_type: Option<Type> = None;
    let closure_state_param_ident = format_ident!("__tool_fn_state_arg_");

    if let Some(FnArg::Typed(PatType { ty, .. })) = func_sig.inputs.first() {
        if let Some(inner_ty) = get_arc_inner_type(ty) {
            concrete_state_type = Some(inner_ty.clone());
            input_iter.next();
            original_fn_invocation_args.push(quote! { #closure_state_param_ident.clone() });
        }
    }

    let mut number_of_json_params = 0;
    for input in input_iter {
        number_of_json_params += 1;
        let param_pat_type = match input {
            FnArg::Typed(pt) => pt,
            FnArg::Receiver(_) => {
                return TokenStream::from(
                    Error::new_spanned(input, "Tool function parameters cannot be `self`")
                        .to_compile_error(),
                );
            }
        };

        let param_name_ident = match &*param_pat_type.pat {
            Pat::Ident(pat_ident) => &pat_ident.ident,
            _ => {
                return TokenStream::from(
                    Error::new_spanned(
                        param_pat_type.clone().pat.clone(),
                        "Tool function parameters must be simple identifiers (e.g., `my_arg: String`)",
                    )
                    .to_compile_error(),
                );
            }
        };
        let param_ty_box = param_pat_type.clone().ty.clone();
        let param_ty_ref = &*param_ty_box;
        let param_name_str = param_name_ident.to_string();
        original_fn_invocation_args.push(quote! { #param_name_ident });

        let (gemini_schema_type_str, gemini_param_is_optional) =
            match rust_type_to_schema_info(param_ty_ref) {
                Ok(info) => info,
                Err(e) => return TokenStream::from(e.to_compile_error()),
            };
        gemini_schema_properties.extend(quote! {
            (#param_name_str.to_string(), ::gemini_live_api::types::Schema {
                schema_type: #gemini_schema_type_str.to_string(),
                ..Default::default()
            }),
        });
        if !gemini_param_is_optional {
            gemini_required_params.push(quote! { #param_name_str.to_string() });
        }

        let (openai_json_type_str, openai_param_is_optional) =
            match rust_type_to_openai_json_schema_type(param_ty_ref) {
                Ok(info) => info,
                Err(e) => return TokenStream::from(e.to_compile_error()),
            };
        openai_schema_properties_map.insert(
            param_name_str.clone(),
            json!({ "type": openai_json_type_str }),
        );
        if !openai_param_is_optional {
            openai_required_params.push(param_name_str.clone());
        }

        let temp_val_ident = format_ident!("__tool_arg_{}_json_val", param_name_str);
        let parsing_code = if gemini_param_is_optional {
            quote! {
                let #temp_val_ident = __args_obj.get(#param_name_str).cloned();
                let #param_name_ident : #param_ty_box = match #temp_val_ident {
                    Some(::serde_json::Value::Null) | None => None,
                    Some(val) => {
                        let val_clone_for_err = val.clone();
                        Some(::serde_json::from_value(val).map_err(|e| format!("Failed to parse optional argument '{}': {} from value {:?}", #param_name_str, e, val_clone_for_err))?)
                    }
                };
            }
        } else {
            quote! {
                let #temp_val_ident = __args_obj.get(#param_name_str).cloned();
                let #param_name_ident : #param_ty_box = match #temp_val_ident {
                    Some(val) => {
                        let val_clone_for_err = val.clone();
                        ::serde_json::from_value(val).map_err(|e| format!("Failed to parse required argument '{}': {} from value {:?}", #param_name_str, e, val_clone_for_err))?
                    }
                    None => return Err(format!("Missing required argument '{}'", #param_name_str)),
                };
            }
        };
        adapter_arg_parsing_code.extend(parsing_code);
    }

    let gemini_schema_map = if gemini_schema_properties.is_empty() {
        quote! { None }
    } else {
        quote! { Some(::std::collections::HashMap::from([#gemini_schema_properties])) }
    };
    let gemini_required_vec = if gemini_required_params.is_empty() {
        quote! { None }
    } else {
        quote! { Some(vec![#(#gemini_required_params),*]) }
    };

    let gemini_parameters_schema = if number_of_json_params > 0 {
        quote! {
            Some(::gemini_live_api::types::Schema {
                schema_type: "OBJECT".to_string(),
                properties: #gemini_schema_map,
                required: #gemini_required_vec,
                ..Default::default()
            })
        }
    } else {
        quote! { None }
    };

    let gemini_declaration_code = quote! {
         fn get_gemini_declaration() -> ::gemini_live_api::types::FunctionDeclaration {
             ::gemini_live_api::types::FunctionDeclaration {
                 name: #func_name_str.to_string(),
                 description: #final_description.to_string(),
                 parameters: #gemini_parameters_schema,
             }
         }
    };

    let openai_parameters_def_obj = if number_of_json_params > 0 {
        json!({
            "type": "object",
            "properties": openai_schema_properties_map,
            "required": openai_required_params
        })
    } else {
        json!({ "type": "object", "properties": {} })
    };

    // <<< MODIFIED HERE for OpenAI Realtime API tool structure >>>
    let openai_tool_def_json = json!({
        "type": "function",
        "name": func_name_str.clone(),
        "description": final_description.clone(),
        "parameters": openai_parameters_def_obj,
    });
    // The above line was:
    // "function": { "name": ..., "description": ..., "parameters": ... }
    // Now name, description, parameters are direct children of the tool object if type is "function".

    let openai_def_str = openai_tool_def_json.to_string();
    let openai_definition_code = quote! {
         fn get_openai_definition() -> ::serde_json::Value {
             ::serde_json::from_str(#openai_def_str)
                .expect("Internal Error: Failed to parse static OpenAI JSON definition")
         }
    };

    let adapter_return_handling_code = match &func_sig.output {
        ReturnType::Default => {
            quote! { __result_invocation_block.await; Ok(::serde_json::Value::Null) }
        }
        ReturnType::Type(_, ty_box) => {
            let ty_ref = &**ty_box;
            if let Some((ok_ty, _err_ty)) = get_result_types(ty_ref) {
                quote! {
                    let __result_val = __result_invocation_block.await;
                    match __result_val {
                        Ok(ok_val) => ::serde_json::to_value(ok_val).map_err(|e| format!("Failed to serialize success result type {}: {}", stringify!(#ok_ty), e)),
                        Err(err_val) => Err(format!("{}", err_val)),
                    }
                }
            } else {
                quote! {
                    let __result_val = __result_invocation_block.await;
                    ::serde_json::to_value(__result_val).map_err(|e| format!("Failed to serialize function result type {}: {}", stringify!(#ty_ref), e))
                }
            }
        }
    };

    let register_tool_fn_name = format_ident!("{}_register_tool", func_name);

    let args_obj_match_block = quote! {
        let __args_obj: ::serde_json::Map<String, ::serde_json::Value> = match __args_json {
            Some(::serde_json::Value::Object(map)) => map,
            None if #number_of_json_params == 0 => ::serde_json::Map::new(),
            None => return Err(format!("Tool '{}' requires arguments (a JSON object), but got null/none.", #func_name_str)),
            Some(other_json_value) => {
                if #number_of_json_params == 0 {
                    ::serde_json::Map::new()
                } else {
                    return Err(format!("Tool '{}' arguments must be a JSON object, got: {:?}", #func_name_str, other_json_value));
                }
            }
        };
    };

    let (builder_generic_param_for_fn, builder_state_generic_decl) =
        if let Some(state_ty) = &concrete_state_type {
            (quote! { #state_ty }, quote! {})
        } else {
            (
                quote! { S_BUILDER_STATE },
                quote! { <S_BUILDER_STATE: Clone + Send + Sync + 'static> },
            )
        };

    let closure_state_param_type_for_fn = if let Some(state_ty) = &concrete_state_type {
        quote! { ::std::sync::Arc<#state_ty> }
    } else {
        quote! { ::std::sync::Arc<S_BUILDER_STATE> }
    };

    let registration_code = quote! {
        pub fn #register_tool_fn_name #builder_state_generic_decl (
            builder: ::gemini_live_api::client::builder::AiClientBuilder<#builder_generic_param_for_fn>
        ) -> ::gemini_live_api::client::builder::AiClientBuilder<#builder_generic_param_for_fn>
        {
            let tool_adapter_closure = ::std::sync::Arc::new(
                move |__args_json: Option<::serde_json::Value>, #closure_state_param_ident: #closure_state_param_type_for_fn|
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<::serde_json::Value, String>> + Send>>
                {
                    Box::pin(async move {
                        #args_obj_match_block
                        #adapter_arg_parsing_code
                        let __result_invocation_block = async { #func_name(#(#original_fn_invocation_args),*).await };
                        #adapter_return_handling_code
                    })
                }
            );

            builder.add_tool_internal(
                #func_name_str.to_string(),
                get_gemini_declaration(),
                get_openai_definition(),
                tool_adapter_closure,
            )
        }
    };

    let output = quote! {
        #func

        #[allow(non_snake_case)]
        mod #func_name {
            use super::*;
            #gemini_declaration_code
            #openai_definition_code
            #registration_code
        }
        pub use #func_name::#register_tool_fn_name;
    };
    output.into()
}
