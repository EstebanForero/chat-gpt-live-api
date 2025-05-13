// examples/common.rs (if you create this file)
use gemini_live_api::ApiProvider;
use std::env;

pub fn get_api_key_and_provider() -> Result<(String, String, ApiProvider), String> {
    dotenvy::dotenv().ok(); // Use dotenvy

    let api_key = env::var("API_KEY")
        .map_err(|_| "API_KEY not set. Please set for Gemini or OpenAI.".to_string())?;

    let provider_str = env::var("API_PROVIDER").unwrap_or_else(|_| "gemini".to_string());

    let (provider, default_model) = match provider_str.to_lowercase().as_str() {
        "gemini" => (
            ApiProvider::Gemini,
            "models/gemini-1.5-flash-latest".to_string(),
        ),
        "openai" => (ApiProvider::OpenAi, "gpt-4o".to_string()), // Use a known OpenAI model
        _ => {
            return Err(format!(
                "Unsupported API_PROVIDER: {}. Choose 'gemini' or 'openai'.",
                provider_str
            ));
        }
    };

    let model_env_key = match provider {
        ApiProvider::Gemini => "GEMINI_MODEL",
        ApiProvider::OpenAi => "OPENAI_MODEL",
    };
    let model_name = env::var(model_env_key).unwrap_or(default_model);

    Ok((api_key, model_name, provider))
}
