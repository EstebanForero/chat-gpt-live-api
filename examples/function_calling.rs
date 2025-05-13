use gemini_live_api::{GeminiLiveClientBuilder, tool_function};
use gemini_live_api::{
    client::{ServerContentContext, UsageMetadataContext},
    types::*,
};
use std::{
    env,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::Notify;
use tracing::{error, info};

#[derive(Clone, Default, Debug)]
struct AppStateWithMutex {
    full_response: Arc<StdMutex<String>>,
    call_count: Arc<StdMutex<u32>>,
    turn_complete_signal: Arc<Notify>,
}

#[tool_function("Calculates the sum of two numbers and increments a counter")]
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
async fn divide(numerator: f64, denominator: f64) -> Result<f64, String> {
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
        "[Handler] Received Usage Metadata: {:?}, current call count from state: {}",
        _ctx.metadata,
        app_state.call_count.lock().unwrap()
    );
}

async fn handle_on_content(ctx: ServerContentContext, app_state: Arc<AppStateWithMutex>) {
    info!("[Handler] Received content: {:?}", ctx.content);
    if let Some(model_turn) = &ctx.content.model_turn {
        for part in &model_turn.parts {
            if let Some(text) = &part.text {
                let mut full_res = app_state.full_response.lock().unwrap();
                *full_res += text;
                *full_res += " ";
            }
        }
    }
    if ctx.content.turn_complete {
        info!("[Handler] Turn complete message received.");
        app_state.turn_complete_signal.notify_one();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    dotenv::dotenv().ok();

    let api_key = env::var("GEMINI_API_KEY").map_err(|_| "GEMINI_API_KEY not set")?;
    let model_name = "models/gemini-2.0-flash-exp".to_string(); // Updated model

    let client_app_state_instance = AppStateWithMutex {
        full_response: Arc::new(StdMutex::new(String::new())),
        call_count: Arc::new(StdMutex::new(0)),
        turn_complete_signal: Arc::new(Notify::new()),
    };

    info!("Configuring Gemini Live Client...");

    let mut builder = GeminiLiveClientBuilder::<AppStateWithMutex>::new_with_state(
        api_key,
        model_name,
        client_app_state_instance.clone(),
    );

    builder = builder.generation_config(GenerationConfig {
        response_modalities: Some(vec![ResponseModality::Text]),
        temperature: Some(0.7),
        ..Default::default()
    });

    builder = builder.system_instruction(Content {
        parts: vec![Part {
            text: Some("You are a helpful assistant. Use tools for calculations.".to_string()),
            ..Default::default()
        }],
        role: Some(Role::System),
        ..Default::default()
    });

    builder = builder.on_server_content(handle_on_content);
    builder = builder.on_usage_metadata(handle_usage_metadata);

    builder = sum_register_tool(builder);
    builder = divide_register_tool(builder);

    info!("Connecting client...");
    let mut client = builder.connect().await?;

    let user_prompt =
        "Please calculate 15.5 + 7.2 for me. Then, divide that sum by 2. Then add 10 + 5.";
    info!("Sending initial prompt: {}", user_prompt);
    client.send_text_turn(user_prompt.to_string(), true).await?;

    info!("Waiting for turn completion or Ctrl+C...");

    let turn_complete_notification = client_app_state_instance.turn_complete_signal.clone();

    tokio::select! {
        _ = turn_complete_notification.notified() => {
            info!("Completion signaled by handler.");
            let final_app_state_arc = client.state();
            let final_text = final_app_state_arc.full_response.lock().unwrap().trim().to_string();
            let final_calls = *final_app_state_arc.call_count.lock().unwrap();
            info!(
                "\n--- Final Text Response (from state) ---\n{}\n--------------------\nTool call count: {}",
                final_text, final_calls
            );
        }
        res = tokio::signal::ctrl_c() => {
            if let Err(e) = res { error!("Failed to listen for Ctrl+C: {}", e); }
            else { info!("Ctrl+C received, initiating shutdown."); }
        }
        _ = tokio::time::sleep(std::time::Duration::from_secs(180)) => {
            error!("Overall interaction timed out after 180 seconds.");
        }
    }

    info!("Shutting down client...");
    client.close().await?;
    info!("Client closed.");

    Ok(())
}
