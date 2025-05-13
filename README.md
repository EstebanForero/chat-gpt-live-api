# Gemini Live API

[![crates.io](https://img.shields.io/crates/v/gemini-live-api.svg)](https://crates.io/crates/gemini-live-api)
[![docs.rs](https://docs.rs/gemini-live-api/badge.svg)](https://docs.rs/gemini-live-api)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
<!-- TODO: Add build status badge once CI is set up -->

A Rust client for real-time, bidirectional communication with Google's Gemini large language models via WebSockets. This crate enables interactive conversations, function calling, and streaming responses, with flexible state management for your application.

It's built on `tokio` for asynchronous operations and `serde` for robust JSON handling.

## Features

*   **Real-time WebSocket Communication:** Establishes a persistent WebSocket connection with the Gemini API.
*   **Robust Function Calling:**
    *   Declare tool functions using the `#[tool_function]` procedural macro.
    *   Automatic JSON schema generation for function parameters (derived from function signature).
    *   Handles server-side function call requests and sends back responses.
*   **Flexible Application State Management:**
    *   The `GeminiLiveClient<S>` is generic over a state type `S`.
    *   Your custom state `S` (wrapped in `Arc<S>`) is accessible within tool functions and can be captured by event handlers.
    *   Supports interior mutability patterns (e.g., `Arc<Mutex<T>>` within your state `S`) for concurrent modifications.
*   **Event-Driven Architecture:**
    *   Register asynchronous handlers for server events like `on_server_content` (for model responses, text, turn completion) and `on_usage_metadata`.
*   **Configurable:**
    *   Set generation parameters (temperature, max tokens, etc.).
    *   Provide system instructions to guide the model's behavior.
*   **Typed API:** Rust structs for Gemini API requests and responses, ensuring type safety.
*   **Asynchronous:** Fully `async/await` based using `tokio`.

