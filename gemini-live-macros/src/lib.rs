extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote, ToTokens};
use serde_json::json; // For building OpenAI tool definition
use syn::{
    parse_macro_input, punctuated::Punctuated, token, AngleBracketedGenericArguments, Attribute,
    Error, FnArg, GenericArgument, Ident, ItemFn, Lit, LitStr, Meta, MetaNameValue, Pat, PatType,
    Path, PathArguments, PathSegment, ReturnType, Type, TypePath, Visibility,
};
use serde_json::Value; // Added explicit use


// --- Helper Functions (get_description, rust_type_to_schema_info, get_result_types, is_arc_type) ---
// (Keep these largely the same as in the previous plan)
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
                    // Trim only leading whitespace per line, keep structure
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
                    "Unsupported doc attribute format. Use /// comment or #[doc = \\"...\\"]",
                ));
            }
        }
    }
    if doc_lines.is_empty() {
        Err(Error::new(
            error_span,
            "Missing tool description: Add a /// doc comment above the function.",
        ))
    } else {
        // Join lines, preserving internal newlines from multi-line comments
        Ok(doc_lines.join("\\n").trim().to_string())
    }
}

fn rust_type_to_schema_info(ty: &Type) -> Option<(String, bool)> {
    // Gemini Schema: returns ("TYPE_ENUM_STRING", is_optional)
     if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            let type_name = segment.ident.to_string();
            if type_name == "Option" {
                if let PathArguments::AngleBracketed(angle_args) = &segment.arguments {
                    if let Some(GenericArgument::Type(inner_ty)) = angle_args.args.first() {
                        // Get inner type schema, mark as optional
                        return rust_type_to_schema_info(inner_ty).map(|(s, _)| (s, true));
                    }
                }
            }
            // Map base types to Gemini Schema types
            match type_name.as_str() {
                "String" => Some(("STRING".to_string(), false)),
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "isize" | "usize" => {
                    Some(("INTEGER".to_string(), false))
                }
                "f32" | "f64" => Some(("NUMBER".to_string(), false)),
                "bool" => Some(("BOOLEAN".to_string(), false)),
                // Add Array/Object support if needed later
                _ => None, // Unsupported type for schema
            }
        } else { None }
    } else { None }
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
            // Check for fully qualified Result or just Result
            if (ident == "Result") || (segments.len() > 1 && segments[segments.len()-2].ident == "result" && segments.last().unwrap().ident == "Result") {
                if args.len() == 2 {
                     if let (Some(GenericArgument::Type(ok_ty)), Some(GenericArgument::Type(err_ty))) =
                        (args.first(), args.last())
                    {
                        return Some((ok_ty, err_ty));
                    }
                } else if args.len() == 1 { // Handle Result<T> which implies Result<T, _>
                     if let Some(GenericArgument::Type(ok_ty)) = args.first() {
                         // We don't know the error type easily here, but we know it's a Result
                         // For serialization purposes, we only care about the Ok type mostly
                         // Let's return a dummy placeholder or handle it in the serialization logic
                         // Returning None here might be safer if we *need* the error type
                          return None; // Can't determine error type from Result<T> alone
                     }
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
                    // Check for std::sync::Arc or ::std::sync::Arc or just Arc
                    if path.segments.len() == 1 { return true; } // `Arc<...>`
                    if path.segments.len() >= 3
                        && path.segments[path.segments.len() - 3].ident == "std"
                        && path.segments[path.segments.len() - 2].ident == "sync" { return true; } // `std::sync::Arc<...>`
                     if path.leading_colon.is_some()
                        && path.segments.len() >= 3
                        && path.segments[0].ident == "std"
                        && path.segments[1].ident == "sync" { return true; } // `::std::sync::Arc<...>`
                }
            }
        }
    }
    false
}

// Helper to convert Rust type to OpenAPI Schema JSON Value
 fn rust_type_to_openai_schema(ty: &Type) -> Option<(Value, bool)> {
     if let Type::Path(type_path) = ty {
         if let Some(segment) = type_path.path.segments.last() {
             let type_name = segment.ident.to_string();
             if type_name == "Option" {
                 if let PathArguments::AngleBracketed(angle_args) = &segment.arguments {
                     if let Some(GenericArgument::Type(inner_ty)) = angle_args.args.first() {
                         // Option doesn't change the type, just makes it not required
                         return rust_type_to_openai_schema(inner_ty).map(|(v, _)| (v, true));
                     }
                 }
             }
             // Map base types to JSON Schema types (strings)
             let schema_type_str = match type_name.as_str() {
                 "String" => "string",
                 "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "isize" | "usize" => "integer",
                 "f32" | "f64" => "number",
                 "bool" => "boolean",
                 // TODO: Add support for Vec -> array, HashMap/struct -> object ?
                 // For now, only primitive types are directly supported.
                 _ => return None, // Unsupported type for schema
             };
             // OpenAI schema needs a "type" field as a string
             return Some((json!({ "type": schema_type_str }), false));
         }
     }
     None
 }


// --- Main Macro Logic ---
#[proc_macro_attribute]
pub fn tool_function(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let func_vis = &func.vis;
    let func_sig = &func.sig;
    let func_name = &func_sig.ident;
    let func_async = func_sig.asyncness.is_some();
    let func_name_str = func_name.to_string(); // Get string name early

    // Use helper for description
    let description = match get_description_from_attrs(&func.attrs, func_name.span()) {
         Ok(desc) => desc,
         Err(e) => return TokenStream::from(e.to_compile_error()),
    };

    // Allow doc attribute for description as well
    let description_from_attr = if !attr.is_empty() {
         match parse_macro_input!(attr as LitStr) {
             lit_str => Some(lit_str.value()),
             // If attr is not a LitStr, it's an error (or handle other formats?)
             // For now, assume LitStr or empty attr
         }
     } else { None };

     // Prefer attribute description if provided, otherwise use doc comment
     let final_description = description_from_attr.unwrap_or(description);


    if !func_async {
        return TokenStream::from(
            Error::new_spanned(func_sig.fn_token, "Tool function must be async")
                .to_compile_error(),
        );
    }

    let mut gemini_schema_properties = quote! {};
    let mut gemini_required_params = Vec::<proc_macro2::TokenStream>::new();
    let mut openai_schema_properties = Vec::new(); // Store ("name", Value) pairs
    let mut openai_required_params = Vec::new();   // Store "name" strings

    let mut adapter_arg_parsing_code = quote! {};
    let mut original_fn_invocation_args = Vec::<proc_macro2::TokenStream>::new();
    let closure_state_param_name = format_ident!("__tool_handler_state_arg_dyn"); // Use consistent name
    let mut input_iter = func_sig.inputs.iter();
    let mut concrete_state_type_for_closure: Option<Type> = None;
    let mut number_of_json_params = 0;

    // --- State Argument Handling ---
    if let Some(FnArg::Typed(PatType { ty, .. })) = func_sig.inputs.first() {
        if is_arc_type(ty) {
            input_iter.next(); // Consume the state argument
            original_fn_invocation_args.push(quote! { state_s }); // Use downcasted state_s

            // Extract concrete state type for downcasting later
            if let Type::Path(type_path) = &**ty {
                if let Some(segment) = type_path.path.segments.last() {
                    if segment.ident == "Arc" {
                        if let PathArguments::AngleBracketed(angle_args) = &segment.arguments {
                            if let Some(GenericArgument::Type(inner_ty)) = angle_args.args.first()
                            {
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

    // --- Parameter Processing ---
    for input in input_iter {
        if let FnArg::Typed(PatType { pat, ty, .. }) = input {
            if let Pat::Ident(pat_ident) = &**pat {
                number_of_json_params += 1;
                let param_name_ident = &pat_ident.ident;
                let param_name_str = param_name_ident.to_string();
                original_fn_invocation_args.push(quote! { #param_name_ident });

                // Gemini Schema
                let (gemini_schema_type_str, param_is_optional) =
                    match rust_type_to_schema_info(ty) {
                        Some(info) => info,
                        None => {
                            return TokenStream::from(
                                Error::new_spanned(
                                    ty,
                                    format!(
                                        "Unsupported parameter type for Gemini schema: {}",
                                        ty.to_token_stream()
                                    ),
                                )
                                .to_compile_error(),
                            );
                        }
                    };
                // Simple schema for now, no description per param yet
                gemini_schema_properties.extend(quote! { (#param_name_str.to_string(), ::gemini_live_api::types::Schema { schema_type: #gemini_schema_type_str.to_string(), ..Default::default() }, ), });
                if !param_is_optional {
                    gemini_required_params.push(quote! { #param_name_str.to_string() });
                }

                // OpenAI Schema
                 let (openai_param_schema_val, _) = // Optionality handled by 'required' list
                    match rust_type_to_openai_schema(ty) {
                        Some(info) => info,
                        None => {
                             return TokenStream::from(
                                Error::new_spanned(
                                    ty,
                                     format!(
                                        "Unsupported parameter type for OpenAI schema: {}",
                                        ty.to_token_stream()
                                    ),
                                )
                                .to_compile_error(),
                            );
                         }
                    };
                 openai_schema_properties.push((param_name_str.clone(), openai_param_schema_val));
                 if !param_is_optional {
                     openai_required_params.push(param_name_str.clone());
                 }

                // Argument Parsing Code (Uses param_is_optional derived from Gemini schema)
                 let temp_val_ident = format_ident!("__tool_arg_{}_json_val", param_name_str);
                 let parsing_code = if param_is_optional {
                     quote! {
                         let #temp_val_ident = args_obj.get(#param_name_str).cloned();
                         // Use serde_json::from_value for robust parsing
                         let #param_name_ident : #ty = match #temp_val_ident {
                              Some(::serde_json::Value::Null) => None, // Treat JSON null as None for Option types
                              Some(val) => Some(::serde_json::from_value(val).map_err(|e| format!("Failed to parse optional argument '{}': {}", #param_name_str, e))?),
                              None => None, // Missing optional field is None
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
                    Error::new_spanned(pat, "Tool function parameters must be simple identifiers (e.g., `a: f64`)").to_compile_error(),
                );
            }
        } else {
            return TokenStream::from(
                Error::new_spanned(input, "Unsupported function argument type (e.g., `self`)").to_compile_error(),
            );
        }
    }

    // --- Build Gemini Declaration ---
    let gemini_schema_map = if gemini_schema_properties.is_empty() { quote! { None } } else { quote! { Some(::std::collections::HashMap::from([#gemini_schema_properties])) } };
    let gemini_required_vec = if gemini_required_params.is_empty() { quote! { None } } else { quote! { Some(vec![#(#gemini_required_params),*]) } };
    let gemini_parameters_schema = if number_of_json_params > 0 {
         quote! { Some(::gemini_live_api::types::Schema { schema_type: "OBJECT".to_string(), properties: #gemini_schema_map, required: #gemini_required_vec, ..Default::default() }) }
    } else {
         quote! { None } // No parameters if function takes none
    };
    // Use a helper function inside the module to avoid static initialization issues
    let gemini_declaration_code = quote! {
         fn get_gemini_declaration() -> ::gemini_live_api::types::FunctionDeclaration {
             // Construct on demand
             ::gemini_live_api::types::FunctionDeclaration {
                 name: #func_name_str.to_string(),
                 description: #final_description.to_string(),
                 parameters: #gemini_parameters_schema,
             }
         }
    };

    // --- Build OpenAI Tool Definition (as JSON Value) ---
    let openai_params_obj = Value::Object(
         openai_schema_properties.into_iter()
             .map(|(name, schema_val)| (name, schema_val))
             .collect()
     );
      // Only include parameters if the function takes arguments
     let openai_parameters_def = if number_of_json_params > 0 {
         json!({
             "type": "object",
             "properties": openai_params_obj,
             "required": openai_required_params,
         })
     } else {
          // OpenAI seems to require an empty object if no params expected
          json!({ "type": "object", "properties": {} })
     };
    let openai_tool_def_json = json!({
        "type": "function",
        "function": {
            "name": func_name_str.clone(), // Use cloned func_name_str
            "description": final_description.clone(), // Use cloned description
            "parameters": openai_parameters_def,
        }
    });
    let openai_def_str = openai_tool_def_json.to_string();
    let openai_definition_code = quote! {
         fn get_openai_definition() -> ::serde_json::Value {
             // Parse the string literal back to Value at runtime to avoid static complexities
             ::serde_json::from_str(#openai_def_str)
                 .expect("Internal Error: Failed to parse static OpenAI JSON definition")
         }
    };


    // --- Adapter Return Handling (Serialize result to JSON Value) ---
    let adapter_return_handling_code = match &func_sig.output {
        ReturnType::Default => {
             // No return value, call the function and return Ok(Value::Null)
            quote! {
                 result_invocation_block.await; // Execute the function call block
                 Ok(::serde_json::Value::Null) // Success, no specific result data
            }
        }
        ReturnType::Type(_, ty) => {
             // Check if the return type is Result<T, E>
            if let Some((ok_ty, _err_ty)) = get_result_types(ty) {
                // Returns Result<T, E>
                quote! {
                     let result_val = result_invocation_block.await; // Execute the function call block
                     match result_val {
                         // Serialize the Ok variant
                         Ok(ok_val) => ::serde_json::to_value(ok_val)
                             .map_err(|e| format!("Failed to serialize success result type {}: {}", stringify!(#ok_ty), e)),
                         // Convert Err variant to error string in JSON response
                         Err(err_val) => {
                             // Return an error string directly, the ToolHandler expects Result<Value, String>
                             Err(format!("{}", err_val)) // Convert the error to string
                         }
                     }
                }
            } else {
                // Returns a plain type T
                quote! {
                     let result_val = result_invocation_block.await; // Execute the function call block
                     // Serialize the plain return value
                     ::serde_json::to_value(result_val)
                          .map_err(|e| format!("Failed to serialize function result type {}: {}", stringify!(#ty), e))
                }
            }
        }
    };

    // --- Registration Function ---
    let register_tool_fn_name = format_ident!("{}_register_tool", func_name);

    let registration_code = if let Some(concrete_state_ty) = concrete_state_type_for_closure {
        // --- Registration function WITH state ---
        quote! {
             #[doc(hidden)]
             #func_vis fn #register_tool_fn_name<S_CLIENT_BUILDER_STATE>(
                 // Builder is generic, but we expect it to hold the concrete state type
                 mut builder: ::gemini_live_api::client::builder::GeminiLiveClientBuilder<S_CLIENT_BUILDER_STATE>
             ) -> ::gemini_live_api::client::builder::GeminiLiveClientBuilder<S_CLIENT_BUILDER_STATE>
              where S_CLIENT_BUILDER_STATE: ::gemini_live_api::client::backend::AppState + Clone + Send + Sync + 'static + std::any::Any
             {
                 // Closure accepts Arc<dyn AppState>
                 let tool_adapter_closure = ::std::sync::Arc::new(
                    move |args: Option<::serde_json::Value>, #closure_state_param_name: ::std::sync::Arc<dyn ::gemini_live_api::client::backend::AppState + Send + Sync>|
                     -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<::serde_json::Value, String>> + Send>>
                    {
                        Box::pin(async move {
                             // Downcast the dyn AppState to the concrete type needed by the function
                             // This requires the state type S to implement 'Any'
                             let state_s: ::std::sync::Arc<#concrete_state_ty> = match #closure_state_param_name.downcast_arc::<#concrete_state_ty>() {
                                 Ok(s) => s,
                                 Err(_) => return Err(format!("Internal error: State type mismatch for tool '{}'. Expected {}.", #func_name_str, stringify!(#concrete_state_ty))),
                             };

                             // Argument parsing logic
                             let args_obj = match args {
                                 Some(::serde_json::Value::Object(map)) => map,
                                 Some(v) if #number_of_json_params > 0 => return Err(format!("Tool '{}' arguments must be a JSON object, got: {:?}", #func_name_str, v)),
                                 Some(_) if #number_of_json_params == 0 => ::serde_json::Map::new(), // Ignore non-object args if none expected
                                 None if #number_of_json_params > 0 => return Err(format!("Tool '{}' requires arguments, but none were provided.", #func_name_str)),
                                 None => ::serde_json::Map::new(), // No args expected or provided
                             };
                             #adapter_arg_parsing_code

                             // Block for the actual function call
                             let result_invocation_block = async {
                                 #func_name(#(#original_fn_invocation_args),*).await // Passes state_s and parsed params
                             };

                             // Process the result using the return handling code
                             #adapter_return_handling_code
                        })
                    }
                 );

                 builder.add_tool_internal(
                     #func_name_str.to_string(),
                     get_gemini_declaration(), // Call helper
                     get_openai_definition(),  // Call helper
                     tool_adapter_closure    // Pass the wrapped closure
                 )
             }
        }
    } else {
        // --- Registration function WITHOUT state ---
        quote! {
             #[doc(hidden)]
             #func_vis fn #register_tool_fn_name<S_CLIENT_BUILDER_STATE>(
                mut builder: ::gemini_live_api::client::builder::GeminiLiveClientBuilder<S_CLIENT_BUILDER_STATE>
             ) -> ::gemini_live_api::client::builder::GeminiLiveClientBuilder<S_CLIENT_BUILDER_STATE>
              where S_CLIENT_BUILDER_STATE: ::gemini_live_api::client::backend::AppState + Clone + Send + Sync + 'static
             {
                 let tool_adapter_closure = ::std::sync::Arc::new(
                     move |args: Option<::serde_json::Value>, _ignored_state: ::std::sync::Arc<dyn ::gemini_live_api::client::backend::AppState + Send + Sync>|
                     -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<::serde_json::Value, String>> + Send>>
                    {
                         Box::pin(async move {
                             // No state downcasting needed

                             // Argument parsing logic
                             let args_obj = match args {
                                  Some(::serde_json::Value::Object(map)) => map,
                                 Some(v) if #number_of_json_params > 0 => return Err(format!("Tool '{}' arguments must be a JSON object, got: {:?}", #func_name_str, v)),
                                 Some(_) if #number_of_json_params == 0 => ::serde_json::Map::new(),
                                 None if #number_of_json_params > 0 => return Err(format!("Tool '{}' requires arguments, but none were provided.", #func_name_str)),
                                 None => ::serde_json::Map::new(),
                             };
                             #adapter_arg_parsing_code

                             // Block for the actual function call (no state arg)
                             let result_invocation_block = async {
                                #func_name(#(#original_fn_invocation_args),*).await
                             };

                             // Process the result using the return handling code
                             #adapter_return_handling_code
                         })
                    }
                 );

                 builder.add_tool_internal(
                     #func_name_str.to_string(),
                     get_gemini_declaration(),
                     get_openai_definition(),
                     tool_adapter_closure
                 )
             }
        }
    };

    // --- Final Output ---
    let output = quote! {
        #func // Original function definition

        // Use a module to contain generated helpers and registration function
        #[allow(non_snake_case)] // Allow module name to match function
        mod #func_name {
             use super::*; // Import the function itself and outer types
             // Needed for downcasting if state is used
              use std::sync::Arc;
              use std::any::Any;
              use ::gemini_live_api::client::backend::AppState; // Make sure AppState is accessible

             #gemini_declaration_code
             #openai_definition_code
             #registration_code
        }
        // Re-export the registration function for easy use
        #func_vis use #func_name::#register_tool_fn_name;
    };

    // eprintln!("{}", output.to_string()); // Debug: Print generated code
    output.into()
}


