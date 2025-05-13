// examples/function_calling.rs
use gemini_live_api::{
    GeminiLiveClientBuilder,
    // Renamed GeminiLiveClient to AiLiveClient in client::handle
    client::{AiLiveClient, ServerContentContext, UsageMetadataContext},
    tool_function,
    types::*,
};
use std::{
    env,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::sync::Notify;
use tracing::{error, info};

#[derive(Clone, Default, Debug)]
struct AppStateWithMutex {
    full_response: Arc<StdMutex<String>>,
    call_count: Arc<StdMutex<u32>>,
    interaction_complete_signal: Arc<Notify>,
}

// No AppState trait impl needed anymore

#[tool_function("Calculates the sum of two numbers")]
async fn sum(state: Arc<AppStateWithMutex>, a: f64, b: f64) -> f64 {
    let mut count = state.call_count.lock().unwrap();
    *count += 1;
    info!(
        "[Tool] sum called (count {}). Args: a={}, b={}",
        *count, a, b
    );
    a + b
}

#[tool_function("Calculates the division of two numbers")]
async fn divide(
    _state_ignored: Arc<AppStateWithMutex>,
    numerator: f64,
    denominator: f64,
) -> Result<f64, String> {
    info!(
        "[Tool] divide called with num={}, den={}",
        numerator, denominator
    );
    if denominator == 0.0 {
        Err("Cannot divide by zero.".to_string())
    } else {
        Ok(numerator / denominator)
    }
}

async fn handle_usage_metadata(_ctx: UsageMetadataContext, app_state: Arc<AppStateWithMutex>) {
    info!(
        "[Handler] OpenAI Usage Metadata: {:?}, call count: {}",
        _ctx.metadata,
        app_state.call_count.lock().unwrap()
    );
}

async fn handle_on_content(ctx: ServerContentContext, app_state: Arc<AppStateWithMutex>) {
    if let Some(text) = ctx.text {
        let mut full_res = app_state.full_response.lock().unwrap();
        *full_res += &text;
        *full_res += " ";
        info!("[Handler] OpenAI Text: {}", text.trim());
    }
    if ctx.is_done {
        // This signals the end of a content part (e.g., final text from a turn)
        info!("[Handler] OpenAI content segment complete.");
        // For function calling, the final response often comes after tool calls.
        // The ModelTurnComplete or ModelGenerationComplete event is more reliable for signaling overall completion.
        // We'll use a dedicated handler for ModelTurnComplete to notify.
    }
}

async fn handle_model_turn_complete(app_state: Arc<AppStateWithMutex>) {
    info!("[Handler] OpenAI ModelTurnComplete received. Signaling main loop.");
    app_state.interaction_complete_signal.notify_one();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    dotenv::dotenv().ok();

    let api_key = env::var("API_KEY").map_err(|_| "OPENAI_API_KEY not set (using API_KEY)")?;
    let model_name = env::var("OPENAI_MODEL_TOOLS").unwrap_or_else(|_| "gpt-4o".to_string()); // e.g. gpt-4-turbo or gpt-4o
    info!("Using OpenAI Model for Tools: {}", model_name);

    let app_state_value = AppStateWithMutex::default();

    info!("Configuring Client for OpenAI Function Calling...");
    let mut builder = GeminiLiveClientBuilder::<AppStateWithMutex>::new_with_model_and_state(
        api_key,
        model_name,
        app_state_value.clone(),
    );

    builder = builder.generation_config(GenerationConfig {
        response_modalities: Some(vec![ResponseModality::Text]),
        temperature: Some(0.7),
        ..Default::default()
    });
    builder = builder.system_instruction(Content {
        parts: vec![Part {
            text: Some("You are an OpenAI assistant that uses tools for calculations.".to_string()),
            ..Default::default()
        }],
        role: Some(Role::System),
        ..Default::default()
    });

    builder = builder.on_server_content(handle_on_content);
    builder = builder.on_usage_metadata(handle_usage_metadata);
    // Add on_model_turn_complete handler
    // This requires adding the method to the builder:
    // builder = builder.on_model_turn_complete(handle_model_turn_complete);

    builder = sum_register_tool(builder);
    builder = divide_register_tool(builder);

    info!("Connecting OpenAI client...");
    let mut client = builder.connect_openai().await?; // Use connect_openai

    let user_prompt =
        "Please calculate 15.5 + 7.2 for me. Then, divide that sum by 2. Then add 10 + 5.";
    info!("Sending initial prompt: {}", user_prompt);
    client.send_text_turn(user_prompt.to_string()).await?;
    // `send_text_turn` for OpenAI now internally calls `request_response`

    info!("Waiting for OpenAI interaction completion or Ctrl+C...");
    let interaction_complete_notification = app_state_value.interaction_complete_signal.clone();
    let timeout_duration = Duration::from_secs(180);

    tokio::select! {
        _ = interaction_complete_notification.notified() => {
            info!("OpenAI interaction completion signaled.");
            let final_text = app_state_value.full_response.lock().unwrap().trim().to_string();
            let final_calls = *app_state_value.call_count.lock().unwrap();
            info!(
                "\n--- OpenAI Final Text Response ---\n{}\n--------------------\nTool call count: {}",
                final_text, final_calls
            );
        }
        _ = tokio::signal::ctrl_c() => { info!("Ctrl+C received."); }
        _ = tokio::time::sleep(timeout_duration) => { error!("Interaction timed out after {} seconds.", timeout_duration.as_secs()); }
    }

    info!("Shutting down OpenAI client...");
    client.close().await?;
    info!("OpenAI Client closed.");
    Ok(())
}
