//! HTTP REST + WebSocket API server for external frontends.
//!
//! Enabled via the `api_enabled` setting. Listens on `127.0.0.1:{api_port}`.
//!
//! ## REST Endpoints
//!
//! - `GET  /api/health`                  - Health check
//! - `GET  /api/status`                  - Current transcription status
//! - `POST /api/transcription/toggle`    - Toggle plain transcription
//! - `POST /api/transcription/toggle-post-process` - Toggle transcription + post-processing
//!   Accepts optional JSON body: `{ "context": "..." }` to inject into `${context}` placeholder
//! - `POST /api/transcription/cancel`    - Cancel current operation
//! - `GET  /api/settings`                - Read current settings (sanitized)
//! - `GET  /api/models`                  - List available models
//! - `GET  /api/models/current`          - Current model info
//! - `GET  /api/audio/input-devices`     - List input devices
//! - `GET  /api/audio/output-devices`    - List output devices
//! - `GET  /api/history`                 - List transcription history
//!
//! ## WebSocket
//!
//! - `GET /api/events` - Upgrade to WebSocket, streams JSON events

use crate::actions::TranscriptionContext;
use crate::audio_toolkit::audio::{list_input_devices, list_output_devices};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::HistoryManager;
use crate::managers::model::ModelManager;
use crate::managers::transcription::TranscriptionManager;
use crate::signal_handle::send_transcription_input;
use crate::utils::cancel_current_operation;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tauri::{AppHandle, Listener, Manager};
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};

/// Shared state passed to all Axum route handlers.
#[derive(Clone)]
struct ApiState {
    app: AppHandle,
    event_tx: broadcast::Sender<String>,
}

/// Generic JSON envelope for API responses.
#[derive(Serialize)]
struct ApiResponse<T: Serialize> {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    fn success(data: T) -> Json<Self> {
        Json(Self {
            ok: true,
            data: Some(data),
            error: None,
        })
    }
}

fn api_error(msg: impl Into<String>) -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiResponse {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }),
    )
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

async fn health() -> impl IntoResponse {
    ApiResponse::success(serde_json::json!({ "status": "ok" }))
}

async fn status(State(state): State<ApiState>) -> impl IntoResponse {
    let is_recording = state
        .app
        .try_state::<Arc<AudioRecordingManager>>()
        .map_or(false, |rm| rm.is_recording());

    let is_model_loaded = state
        .app
        .try_state::<Arc<TranscriptionManager>>()
        .map_or(false, |tm| tm.is_model_loaded());

    let current_model = state
        .app
        .try_state::<Arc<TranscriptionManager>>()
        .and_then(|tm| tm.get_current_model());

    ApiResponse::success(serde_json::json!({
        "is_recording": is_recording,
        "is_model_loaded": is_model_loaded,
        "current_model": current_model,
    }))
}

async fn toggle_transcription(State(state): State<ApiState>) -> impl IntoResponse {
    send_transcription_input(&state.app, "transcribe", "HTTP API");
    ApiResponse::success(serde_json::json!({ "action": "toggle_transcription" }))
}

/// Optional JSON body for the toggle-post-process endpoint.
#[derive(Deserialize, Default)]
struct PostProcessRequest {
    /// Additional context injected into the `${context}` prompt placeholder.
    #[serde(default)]
    context: Option<String>,
}

async fn toggle_post_process(
    State(state): State<ApiState>,
    body: Option<Json<PostProcessRequest>>,
) -> impl IntoResponse {
    // Store the per-recording context (if any) so the post-processing
    // pipeline can pick it up.
    if let Some(Json(req)) = body {
        if let Some(ctx) = req.context.filter(|s| !s.is_empty()) {
            if let Some(tc) = state.app.try_state::<TranscriptionContext>() {
                tc.set(ctx);
            }
        }
    }

    send_transcription_input(&state.app, "transcribe_with_post_process", "HTTP API");
    ApiResponse::success(serde_json::json!({ "action": "toggle_post_process" }))
}

async fn cancel(State(state): State<ApiState>) -> impl IntoResponse {
    cancel_current_operation(&state.app);
    ApiResponse::success(serde_json::json!({ "action": "cancel" }))
}

async fn get_settings(
    State(state): State<ApiState>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let settings = crate::settings::get_settings(&state.app);

    // Sanitize: strip API keys
    let mut value = serde_json::to_value(&settings).map_err(|e| api_error(e.to_string()))?;
    if let Some(obj) = value.as_object_mut() {
        obj.remove("post_process_api_keys");
    }

    Ok(ApiResponse::success(value))
}

async fn get_models(
    State(state): State<ApiState>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let mm = state
        .app
        .try_state::<Arc<ModelManager>>()
        .ok_or_else(|| api_error("ModelManager not initialized"))?;

    let models = mm.get_available_models();
    Ok(ApiResponse::success(models))
}

async fn get_current_model(
    State(state): State<ApiState>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let tm = state
        .app
        .try_state::<Arc<TranscriptionManager>>()
        .ok_or_else(|| api_error("TranscriptionManager not initialized"))?;

    let model_id = tm.get_current_model();
    let settings = crate::settings::get_settings(&state.app);

    Ok(ApiResponse::success(serde_json::json!({
        "loaded_model": model_id,
        "selected_model": settings.selected_model,
    })))
}

async fn get_input_devices() -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let devices = list_input_devices().map_err(|e| api_error(e.to_string()))?;
    let result: Vec<serde_json::Value> = devices
        .into_iter()
        .map(|d| {
            serde_json::json!({
                "index": d.index,
                "name": d.name,
                "is_default": d.is_default,
            })
        })
        .collect();
    Ok(ApiResponse::success(result))
}

async fn get_output_devices() -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let devices = list_output_devices().map_err(|e| api_error(e.to_string()))?;
    let result: Vec<serde_json::Value> = devices
        .into_iter()
        .map(|d| {
            serde_json::json!({
                "index": d.index,
                "name": d.name,
                "is_default": d.is_default,
            })
        })
        .collect();
    Ok(ApiResponse::success(result))
}

async fn get_history(
    State(state): State<ApiState>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let hm = state
        .app
        .try_state::<Arc<HistoryManager>>()
        .ok_or_else(|| api_error("HistoryManager not initialized"))?;

    let entries = hm
        .get_history_entries()
        .await
        .map_err(|e| api_error(e.to_string()))?;
    Ok(ApiResponse::success(entries))
}

// ---------------------------------------------------------------------------
// WebSocket event streaming
// ---------------------------------------------------------------------------

async fn ws_events(ws: WebSocketUpgrade, State(state): State<ApiState>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: ApiState) {
    let mut rx = state.event_tx.subscribe();
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(30));

    // Send initial status on connect
    let is_recording = state
        .app
        .try_state::<Arc<AudioRecordingManager>>()
        .map_or(false, |rm| rm.is_recording());
    let current_model = state
        .app
        .try_state::<Arc<TranscriptionManager>>()
        .and_then(|tm| tm.get_current_model());

    let welcome = serde_json::json!({
        "event": "connected",
        "is_recording": is_recording,
        "current_model": current_model,
    });
    let _ = socket.send(Message::Text(welcome.to_string().into())).await;
    info!("WebSocket client connected");

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(text) => {
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break; // client disconnected
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("WebSocket client lagged, dropped {n} events");
                    }
                    Err(_) => break, // channel closed
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    _ => {} // ignore other messages from client
                }
            }
            _ = ping_interval.tick() => {
                // Keep the connection alive during long recording sessions
                if socket.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
        }
    }

    info!("WebSocket client disconnected");
}

// ---------------------------------------------------------------------------
// Event bridge: Tauri events -> broadcast channel
// ---------------------------------------------------------------------------

fn setup_event_bridge(app: &AppHandle, tx: &broadcast::Sender<String>) {
    let events_to_forward = [
        "model-state-changed",
        "model-download-progress",
        "model-download-complete",
        "model-download-cancelled",
        "model-deleted",
        "history-updated",
    ];

    for event_name in events_to_forward {
        let tx = tx.clone();
        let name = event_name.to_string();
        app.listen(event_name, move |event| {
            let payload = event.payload();
            let json = serde_json::json!({
                "event": name,
                "data": serde_json::from_str::<serde_json::Value>(payload).unwrap_or(serde_json::Value::Null),
            });
            let _ = tx.send(json.to_string());
        });
    }
}

// ---------------------------------------------------------------------------
// Public interface
// ---------------------------------------------------------------------------

/// Managed handle for the API server.
///
/// This is registered once via `app.manage()` at startup and supports
/// starting, stopping, and restarting the server without re-registering
/// the Tauri state.
pub struct ApiServerHandle {
    shutdown_tx: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl ApiServerHandle {
    /// Create a new empty (stopped) handle.
    pub fn new() -> Self {
        Self {
            shutdown_tx: std::sync::Mutex::new(None),
        }
    }

    /// Gracefully shut down the running API server, if any.
    ///
    /// Safe to call multiple times or when no server is running.
    pub fn shutdown(&self) {
        if let Some(tx) = self.shutdown_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }

    /// Start (or restart) the API server on the given port.
    ///
    /// If a server is already running it is shut down first.
    /// Returns `Ok(())` on success, or an error string if the port
    /// cannot be bound.
    pub fn start(&self, app: &AppHandle, port: u16) -> Result<(), String> {
        // Stop any previously running server
        self.shutdown();

        let (event_tx, _) = broadcast::channel::<String>(256);
        setup_event_bridge(app, &event_tx);

        let state = ApiState {
            app: app.clone(),
            event_tx,
        };

        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        let router = Router::new()
            .route("/api/health", get(health))
            .route("/api/status", get(status))
            .route("/api/transcription/toggle", post(toggle_transcription))
            .route(
                "/api/transcription/toggle-post-process",
                post(toggle_post_process),
            )
            .route("/api/transcription/cancel", post(cancel))
            .route("/api/settings", get(get_settings))
            .route("/api/models", get(get_models))
            .route("/api/models/current", get(get_current_model))
            .route("/api/audio/input-devices", get(get_input_devices))
            .route("/api/audio/output-devices", get(get_output_devices))
            .route("/api/history", get(get_history))
            .route("/api/events", get(ws_events))
            .layer(cors)
            .with_state(state);

        let addr = SocketAddr::from(([127, 0, 0, 1], port));

        // Bind synchronously via a blocking std TcpListener so we can
        // report failures immediately to the caller.
        let std_listener = std::net::TcpListener::bind(addr)
            .map_err(|e| format!("Failed to bind API server to {addr}: {e}"))?;
        std_listener
            .set_nonblocking(true)
            .map_err(|e| format!("Failed to set non-blocking on listener: {e}"))?;

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        // Store the new shutdown sender
        *self.shutdown_tx.lock().unwrap() = Some(shutdown_tx);

        // Spawn the server on Tauri's async runtime (not tokio::spawn directly,
        // which panics because there is no standalone Tokio reactor).
        tauri::async_runtime::spawn(async move {
            let listener =
                tokio::net::TcpListener::from_std(std_listener).expect("from_std listener");
            info!("API server listening on http://{addr}");

            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                    info!("API server shutting down");
                })
                .await
                .unwrap_or_else(|e| error!("API server error: {e}"));
        });

        Ok(())
    }
}
