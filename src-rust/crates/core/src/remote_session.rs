//! Remote session sync — mirrors src/remote/RemoteSessionManager.ts
//! and src/remote/SessionsWebSocket.ts.
//!
//! Manages background synchronization of local session transcripts
//! with the Claude.ai cloud API.

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A cloud session summary (from the list API).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudSession {
    pub id: String,
    pub title: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub project: Option<String>,
    pub message_count: u64,
}

/// Events emitted by the remote session WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    SessionCreated(CloudSession),
    SessionUpdated(CloudSession),
    SessionDeleted { id: String },
}

// ---------------------------------------------------------------------------
// Remote Session Manager
// ---------------------------------------------------------------------------

/// Manages remote session listing and background sync.
pub struct RemoteSessionManager {
    base_url: String,
    access_token: String,
    /// Channel to emit SessionEvents to the TUI.
    _event_tx: mpsc::Sender<SessionEvent>,
}

impl RemoteSessionManager {
    pub fn new(access_token: String) -> (Self, mpsc::Receiver<SessionEvent>) {
        let (tx, rx) = mpsc::channel(64);
        (
            Self {
                base_url: String::new(), // Disabled — Uppli Code uses API keys, not cloud sessions
                access_token,
                _event_tx: tx,
            },
            rx,
        )
    }

    /// Fetch the list of remote sessions for the current user.
    pub async fn list_sessions(&self) -> Result<Vec<CloudSession>, String> {
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{}/api/sessions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| format!("HTTP error: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("API error: {}", resp.status()));
        }

        resp.json::<Vec<CloudSession>>()
            .await
            .map_err(|e| format!("Parse error: {e}"))
    }

    /// Push a transcript entry to the cloud for `session_id`.
    pub async fn push_transcript_entry(
        &self,
        session_id: &str,
        entry_json: &str,
    ) -> Result<(), String> {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!(
                "{}/api/sessions/{}/messages",
                self.base_url, session_id
            ))
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .body(entry_json.to_string())
            .send()
            .await
            .map_err(|e| format!("HTTP error: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("API error: {}", resp.status()));
        }
        Ok(())
    }

    /// Start background sync loop: pushes local transcript to cloud every 30s.
    /// Returns a JoinHandle; caller should keep it alive.
    pub fn start_background_sync(
        self: std::sync::Arc<Self>,
        session_id: String,
        transcript_path: std::path::PathBuf,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            let mut last_sync_len = 0usize;

            loop {
                interval.tick().await;
                // Read transcript file and find new entries since last sync
                if let Ok(content) = tokio::fs::read_to_string(&transcript_path).await {
                    let lines: Vec<&str> = content.lines().collect();
                    if lines.len() > last_sync_len {
                        for line in &lines[last_sync_len..] {
                            if !line.is_empty() {
                                let _ = self.push_transcript_entry(&session_id, line).await;
                            }
                        }
                        last_sync_len = lines.len();
                    }
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Sessions WebSocket
// ---------------------------------------------------------------------------

/// WebSocket client for real-time session events.
pub struct SessionsWebSocket {
    pub ws_url: String,
    pub access_token: String,
}

impl SessionsWebSocket {
    pub fn new(access_token: String) -> Self {
        Self {
            ws_url: String::new(), // Disabled — Uppli Code uses API keys, not cloud sessions
            access_token,
        }
    }

    /// Connect to the sessions WebSocket and emit events.
    /// This is a stub — real implementation requires a WS client crate (tokio-tungstenite).
    pub async fn connect(&self, _event_tx: mpsc::Sender<SessionEvent>) -> Result<(), String> {
        // Stub: in production, connect to self.ws_url with Bearer auth,
        // deserialize incoming JSON as SessionEvent, send via event_tx.
        tracing::info!(
            url = %self.ws_url,
            "SessionsWebSocket stub: real implementation requires tokio-tungstenite"
        );
        Ok(())
    }
}
