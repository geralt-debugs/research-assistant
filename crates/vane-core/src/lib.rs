use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use thiserror::Error;

uniffi::setup_scaffolding!();

const API_BASE: &str = "https://ollama.com/api";
const DEFAULT_LOCAL_BASE: &str = "http://127.0.0.1:11434";

/// Sink for live progress updates while a research request runs. The Tauri
/// shell pushes these to the webview; UniFFI callers use the returned
/// `ResearchResponse` from `research()` directly.
pub type StreamSink = Arc<dyn Fn(StreamEvent) + Send + Sync>;

/// Which backend serves a model. Cloud models run on ollama.com; local models
/// run against a user-configured Ollama server (default `127.0.0.1:11434`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "camelCase")]
pub enum Endpoint {
    Cloud,
    Local,
}

/// A model listed from any backend. `endpoint` tells research routing which
/// host to send the turn to.
#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub endpoint: Endpoint,
    pub name: String,
    pub parameter_size: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl ModelInfo {
    pub fn supports_vision(&self) -> bool {
        self.capabilities
            .iter()
            .any(|capability| capability == "vision")
    }
}

/// Composite model identifier used across the UI and settings storage:
/// `cloud:<name>` or `local:<name>`.
pub fn encode_model_id(endpoint: Endpoint, name: &str) -> String {
    let prefix = match endpoint {
        Endpoint::Cloud => "cloud",
        Endpoint::Local => "local",
    };
    format!("{prefix}:{name}")
}

/// Split a composite `cloud:` / `local:` id into its parts. Bare names are
/// treated as cloud models for backwards compatibility with old settings.
pub fn decode_model_id(model: &str) -> (Endpoint, String) {
    if let Some(name) = model.strip_prefix("local:") {
        (Endpoint::Local, name.to_string())
    } else if let Some(name) = model.strip_prefix("cloud:") {
        (Endpoint::Cloud, name.to_string())
    } else {
        (Endpoint::Cloud, model.to_string())
    }
}

/// Base URL of the local Ollama server, defaulting to localhost.
pub fn local_base_url(settings: &AppSettings) -> String {
    let raw = settings.local_base_url.trim().to_string();
    if raw.is_empty() {
        return DEFAULT_LOCAL_BASE.to_string();
    }
    let with_scheme = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw
    } else {
        format!("http://{raw}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

// ---------------------------------------------------------------------------
// Data records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct CloudModel {
    pub name: String,
    pub parameter_size: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl CloudModel {
    pub fn supports_vision(&self) -> bool {
        self.capabilities
            .iter()
            .any(|capability| capability == "vision")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub api_key: String,
    pub model: String,
    pub vision_model: Option<String>,
    pub mode: String,
    /// Base URL of the local Ollama server (e.g. `http://192.168.1.10:11434`).
    #[serde(default)]
    pub local_base_url: String,
    /// When true, `/api/tags`-listed local models are used directly for
    /// research without a cloud key.
    #[serde(default)]
    pub local_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct SearchSource {
    pub title: String,
    pub url: String,
    pub content: String,
}

/// One turn's model identity: composite id plus resolved endpoint so callers
/// can label answers and route follow-ups.
#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ModelResolution {
    pub model: String,
    pub endpoint: Endpoint,
}

/// A resolved model plus the client used to reach its backend.
struct ResolvedClient {
    model_name: String,
    endpoint: Endpoint,
    cloud: Option<ResearchClient>,
}

impl ResolvedClient {
    fn from(settings: &AppSettings, model_id: &str) -> Result<Self, ResearchError> {
        let (endpoint, name) = decode_model_id(model_id);
        if name.trim().is_empty() {
            return Err(ResearchError::InvalidConfig("select a model".to_string()));
        }
        match endpoint {
            Endpoint::Cloud => {
                let key = settings.api_key.trim().to_string();
                if key.is_empty() {
                    return Err(ResearchError::InvalidConfig(
                        "this model runs on Ollama Cloud; add your API key in Settings".to_string(),
                    ));
                }
                Ok(Self {
                    model_name: name.clone(),
                    endpoint,
                    cloud: Some(ResearchClient::from_parts(key, name)?),
                })
            }
            Endpoint::Local => Ok(Self {
                model_name: name,
                endpoint,
                cloud: None,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ResearchResponse {
    pub message: String,
    pub sources: Vec<SearchSource>,
    pub model: String,
    pub thinking: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StreamEvent {
    /// Which pipeline stage is running right now.
    Phase { phase: String },
    /// Incremental reasoning trace (thinking-capable models only).
    ThinkingDelta { delta: String },
    /// Incremental answer text.
    AnswerDelta { delta: String },
    /// Final payload; answer/thinking are the complete strings.
    Done(ResearchResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct EncodedSettings {
    pub data: String,
}

#[derive(Debug, Error, uniffi::Error)]
pub enum ResearchError {
    #[error("Invalid API configuration: {0}")]
    InvalidConfig(String),
    #[error("Could not reach Ollama Cloud: {0}")]
    Network(String),
    #[error("Ollama Cloud returned an invalid response: {0}")]
    InvalidResponse(String),
    #[error("Ollama Cloud rejected the request: {0}")]
    Server(String),
    #[error("Could not encrypt settings: {0}")]
    Crypto(String),
}

// ---------------------------------------------------------------------------
// Public UniFFI API
// ---------------------------------------------------------------------------

#[derive(uniffi::Object)]
pub struct ResearchClient {
    api_key: String,
    model: String,
    client: Client,
}

#[uniffi::export]
impl ResearchClient {
    #[uniffi::constructor]
    pub fn new(api_key: String, model: String) -> Result<Arc<Self>, ResearchError> {
        let key = api_key.trim().to_string();
        if key.is_empty() {
            return Err(ResearchError::InvalidConfig(
                "the API key cannot be empty".to_string(),
            ));
        }
        if model.trim().is_empty() {
            return Err(ResearchError::InvalidConfig(
                "select a model".to_string(),
            ));
        }
        Ok(Arc::new(Self::from_parts(key, model)?))
    }

    /// List cloud-capable models from Ollama Cloud, including each model's
    /// advertised capabilities (fetched via `/api/show`).
    pub async fn models(&self) -> Result<Vec<CloudModel>, ResearchError> {
        let response = self
            .client
            .get(format!("{API_BASE}/tags"))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|e| ResearchError::Network(e.to_string()))?;

        if response.status().as_u16() == 401 {
            return Err(ResearchError::Server(
                "invalid API key; check your key at ollama.com/settings/keys".to_string(),
            ));
        }
        if !response.status().is_success() {
            return Err(server_error(response).await);
        }

        let payload: TagsResponse = response
            .json()
            .await
            .map_err(|e| ResearchError::InvalidResponse(e.to_string()))?;

        let mut models: Vec<CloudModel> = payload
            .models
            .into_iter()
            .filter(|m| is_cloud_chat_model(&m.name))
            .map(|m| CloudModel {
                name: m.name,
                parameter_size: m.details.parameter_size,
                capabilities: Vec::new(),
            })
            .collect();

        // `/api/tags` does not expose capabilities; resolve them per model via
        // `/api/show` concurrently and tolerate individual failures.
        let capability_jobs: Vec<_> = models
            .iter()
            .map(|m| fetch_capabilities(&self.client, &self.api_key, &m.name))
            .collect();
        let capability_lists = futures::future::join_all(capability_jobs).await;
        for (model, capabilities) in models.iter_mut().zip(capability_lists) {
            if let Ok(capabilities) = capabilities {
                model.capabilities = capabilities;
            }
        }

        Ok(models)
    }

    /// Perform a full research step: web search, fetch, synthesise.
    ///
    /// If `images` is non-empty and `known_models` show that the primary model
    /// lacks the `vision` capability, the request is automatically routed to
    /// `fallback_vision_model` (when provided) or the first known model with
    /// vision support. If no vision-capable model can be found, a clear error
    /// is returned instead of silently dropping the images.
    pub async fn research(
        &self,
        query: String,
        mode: String,
        history: Vec<HistoryEntry>,
        images: Vec<String>,
        fallback_vision_model: Option<String>,
        known_models: Vec<CloudModel>,
    ) -> Result<ResearchResponse, ResearchError> {
        self.research_stream(query, mode, history, images, fallback_vision_model, known_models, None)
            .await
    }
}

impl ResearchClient {
    fn from_parts(api_key: String, model: String) -> Result<Self, ResearchError> {
        Ok(Self {
            api_key,
            model,
            client: http_client()?,
        })
    }

    /// Build a client from stored settings without key validation: local
    /// Ollama turns never need a cloud key. Callers must still ensure a
    /// model is configured.
    pub fn for_shell(settings: &AppSettings) -> Result<Self, ResearchError> {
        Ok(Self {
            api_key: settings.api_key.clone(),
            model: settings.model.clone(),
            client: http_client()?,
        })
    }

    /// Same as `research`, but streams progress events through the sink:
    /// pipeline phases, thinking deltas, and answer deltas. Kept out of the
    /// UniFFI surface — native hosts (Tauri shell) call it directly; foreign
    /// bindings use the returned `ResearchResponse` instead.
    #[allow(clippy::too_many_arguments)]
    pub async fn research_stream(
        &self,
        query: String,
        mode: String,
        history: Vec<HistoryEntry>,
        images: Vec<String>,
        fallback_vision_model: Option<String>,
        known_models: Vec<CloudModel>,
        sink: Option<StreamSink>,
    ) -> Result<ResearchResponse, ResearchError> {
        validate_research(&query, &mode)?;
        let images: Vec<String> = sanitize_images(images);

        let model = if images.is_empty() {
            self.model.clone()
        } else {
            choose_vision_model(&self.model, &fallback_vision_model, &known_models)?
        };
        let thinking = known_models
            .iter()
            .find(|m| m.name == model)
            .map(supports_thinking)
            .unwrap_or(false);

        let mut response = ResearchResponse {
            message: String::new(),
            sources: Vec::new(),
            model: model.clone(),
            thinking: String::new(),
        };

        if images.is_empty() {
            emit(&sink, StreamEvent::Phase { phase: "planning".to_string() });
            let search_terms =
                synthesize_search_terms(&self.client, &self.api_key, &model, &query, &history).await;
            let mut sources = Vec::new();

            emit(&sink, StreamEvent::Phase { phase: "searching".to_string() });

            // Speed mode fetches one query at a time; balanced/quality fan
            // out over up to 3 queries concurrently.
            let selected_terms: Vec<String> = search_terms.iter().take(3).cloned().collect();
            if mode == "speed" {
                for term in &selected_terms {
                    if let Ok(mut list) = web_search(term.clone(), 5, self.api_key.clone()).await {
                        sources.append(&mut list);
                    }
                }
            } else {
                let jobs: Vec<_> = selected_terms
                    .iter()
                    .map(|term| web_search(term.clone(), 6, self.api_key.clone()))
                    .collect();
                for mut list in futures::future::join_all(jobs).await.into_iter().flatten() {
                    sources.append(&mut list);
                }
            }

            // Deduplicate by URL, keep up to 12.
            sources.dedup_by(|a, b| a.url == b.url);
            sources.truncate(12);
            response.sources = sources;

            let context = build_context(&response.sources, &mode);
            emit(&sink, StreamEvent::Phase { phase: "writing".to_string() });
            chat_with_context(
                &self.client,
                &self.api_key,
                &model,
                &query,
                &context,
                &history,
                &[],
                thinking,
                &sink,
                &mut response,
            )
            .await?;
        } else {
            // Image turns: the vision model sees the pictures, the question,
            // and the conversation history. Web search is intentionally
            // skipped — sources stay relevant to the previous turns only.
            emit(&sink, StreamEvent::Phase { phase: "writing".to_string() });
            chat_with_context(
                &self.client,
                &self.api_key,
                &model,
                &query,
                "",
                &history,
                &images,
                thinking,
                &sink,
                &mut response,
            )
            .await?;
        }

        emit(&sink, StreamEvent::Done(response.clone()));
        Ok(response)
    }

    /// Unified research entry point used by the Tauri shell: routes to cloud
    /// or a local Ollama server based on the composite model id.
    #[allow(clippy::too_many_arguments)]
    pub async fn research_any(
        &self,
        settings: &AppSettings,
        query: String,
        mode: String,
        history: Vec<HistoryEntry>,
        images: Vec<String>,
        known_models: Vec<ModelInfo>,
        sink: Option<StreamSink>,
    ) -> Result<ResearchResponse, ResearchError> {
        let images = sanitize_images(images);
        let (primary_endpoint, primary_name) = decode_model_id(&self.model);

        let resolved = resolve_turn_model(
            primary_endpoint,
            &primary_name,
            settings,
            &images,
            &known_models,
        )?;

        let response = match resolved.endpoint {
            Endpoint::Cloud => {
                let cloud = resolved
                    .cloud
                    .ok_or_else(|| ResearchError::InvalidConfig("missing cloud client".to_string()))?;
                let cloud_known: Vec<CloudModel> = known_models
                    .iter()
                    .filter(|m| m.endpoint == Endpoint::Cloud)
                    .map(|m| CloudModel {
                        name: m.name.clone(),
                        parameter_size: m.parameter_size.clone(),
                        capabilities: m.capabilities.clone(),
                    })
                    .collect();
                cloud
                    .research_stream(
                        query,
                        mode,
                        history,
                        images,
                        None,
                        cloud_known,
                        sink,
                    )
                    .await?
            }
            Endpoint::Local => {
                // Local turns can still be grounded with Ollama Cloud's web
                // search when an API key is available. Image turns keep the
                // vision-only pipeline.
                let api_key = settings.api_key.trim();
                let context_sources = if images.is_empty() && !api_key.is_empty() {
                    emit(&sink, StreamEvent::Phase { phase: "planning".to_string() });
                    let search_terms = synthesize_search_terms_local(
                        &local_base_url(settings),
                        &resolved.model_name,
                        &query,
                        &history,
                    )
                    .await;
                    emit(&sink, StreamEvent::Phase { phase: "searching".to_string() });
                    let mut sources = Vec::new();
                    let selected_terms: Vec<String> = search_terms.iter().take(3).cloned().collect();
                    if mode == "speed" {
                        for term in &selected_terms {
                            if let Ok(mut list) = web_search(term.clone(), 5, api_key.to_string()).await {
                                sources.append(&mut list);
                            }
                        }
                    } else {
                        let jobs: Vec<_> = selected_terms
                            .iter()
                            .map(|term| web_search(term.clone(), 6, api_key.to_string()))
                            .collect();
                        for mut list in futures::future::join_all(jobs).await.into_iter().flatten() {
                            sources.append(&mut list);
                        }
                    }
                    sources.dedup_by(|a, b| a.url == b.url);
                    sources.truncate(12);
                    Some(sources)
                } else {
                    None
                };

                let mut response = local_research(
                    &local_base_url(settings),
                    &resolved.model_name,
                    &query,
                    &history,
                    &images,
                    &known_models,
                    context_sources.clone(),
                    sink,
                )
                .await?;
                if let Some(sources) = context_sources {
                    response.sources = sources;
                }
                response
            }
        };
        Ok(response)
    }
}

/// Pick the endpoint/model for this turn: images route to a vision-capable
/// model on the same endpoint (or the other endpoint when the primary side
/// has none), text turns stay on the primary model.
fn resolve_turn_model(
    primary_endpoint: Endpoint,
    primary_name: &str,
    settings: &AppSettings,
    images: &[String],
    known: &[ModelInfo],
) -> Result<ResolvedClient, ResearchError> {
    let has_key = !settings.api_key.trim().is_empty();

    if images.is_empty() {
        return ResolvedClient::from(
            settings,
            &encode_model_id(primary_endpoint, primary_name),
        );
    }

    // Prefer a vision model on the primary endpoint.
    if let Some(model) = known
        .iter()
        .find(|m| m.endpoint == primary_endpoint && m.name == primary_name && m.supports_vision())
    {
        return ResolvedClient::from(settings, &encode_model_id(model.endpoint, &model.name));
    }
    if let Some(model) = known
        .iter()
        .find(|m| m.endpoint == primary_endpoint && m.supports_vision())
    {
        return ResolvedClient::from(settings, &encode_model_id(model.endpoint, &model.name));
    }

    // No vision on the primary endpoint; try the other one when usable.
    let other = match primary_endpoint {
        Endpoint::Cloud => Endpoint::Local,
        Endpoint::Local => Endpoint::Cloud,
    };
    let other_usable = match other {
        Endpoint::Cloud => has_key,
        Endpoint::Local => settings.local_enabled,
    };
    if other_usable {
        if let Some(model) = known
            .iter()
            .find(|m| m.endpoint == other && m.supports_vision())
        {
            return ResolvedClient::from(settings, &encode_model_id(model.endpoint, &model.name));
        }
    }

    Err(ResearchError::InvalidConfig(format!(
        "the selected model ({primary_name}) does not support images and no vision-capable model is available; choose a vision model in Settings"
    )))
}

/// Run a research turn against a local Ollama server. When `context_sources`
/// is non-empty the answer is grounded in those sources (fetched via Ollama
/// Cloud web search); otherwise the model answers from its own knowledge.
#[allow(clippy::too_many_arguments)]
async fn local_research(
    base_url: &str,
    model: &str,
    query: &str,
    history: &[HistoryEntry],
    images: &[String],
    known: &[ModelInfo],
    context_sources: Option<Vec<SearchSource>>,
    sink: Option<StreamSink>,
) -> Result<ResearchResponse, ResearchError> {
    let client = http_client()?;
    let thinking = known
        .iter()
        .find(|m| m.endpoint == Endpoint::Local && m.name == model)
        .map(supports_thinking_model)
        .unwrap_or(false);

    let mut response = ResearchResponse {
        message: String::new(),
        sources: Vec::new(),
        model: encode_model_id(Endpoint::Local, model),
        thinking: String::new(),
    };

    let context = match &context_sources {
        Some(sources) if !sources.is_empty() => build_context(sources, "balanced"),
        _ => String::new(),
    };

    emit(&sink, StreamEvent::Phase { phase: "writing".to_string() });
    local_chat(
        &client,
        base_url,
        model,
        query,
        &context,
        history,
        images,
        thinking,
        &sink,
        &mut response,
    )
    .await?;

    emit(&sink, StreamEvent::Done(response.clone()));
    Ok(response)
}

fn supports_thinking_model(model: &ModelInfo) -> bool {
    model
        .capabilities
        .iter()
        .any(|capability| capability == "thinking")
}

/// Build a chat request body for Ollama's `/api/chat` shared by cloud and
/// local endpoints (`think` is only honoured by thinking-capable models).
fn chat_body(
    model: &str,
    messages: &[serde_json::Value],
    thinking: bool,
    num_ctx: usize,
) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "think": thinking,
        "options": { "temperature": 0.2, "num_ctx": num_ctx }
    })
}

/// Stream a chat completion from a local Ollama server.
#[allow(clippy::too_many_arguments)]
async fn local_chat(
    client: &Client,
    base_url: &str,
    model: &str,
    query: &str,
    context: &str,
    history: &[HistoryEntry],
    images: &[String],
    thinking: bool,
    sink: &Option<StreamSink>,
    out: &mut ResearchResponse,
) -> Result<(), ResearchError> {
    let system = if images.is_empty() {
        "You are a research assistant. Answer the user's question using the provided sources. \
         Be precise, cite sources with inline URLs in markdown, and highlight uncertainty. \
         If sources conflict, say so. Keep answers focused and complete."
    } else {
        "You are a research assistant with vision. Describe and analyse the attached images in the \
         context of the user's question. Reference the earlier conversation when relevant. \
         Keep answers focused and complete."
    };

    let mut messages = vec![serde_json::json!({"role": "system", "content": system})];
    for entry in history.iter().rev().take(10).rev() {
        let role = if entry.role == "assistant" { "assistant" } else { "user" };
        messages.push(serde_json::json!({"role": role, "content": entry.content}));
    }

    let content = if context.is_empty() {
        query.to_string()
    } else {
        format!("Question: {query}\n\nSources:\n{context}")
    };

    let mut user_message = serde_json::json!({"role": "user", "content": content});
    if !images.is_empty() {
        user_message["images"] = serde_json::json!(images);
    }
    messages.push(user_message);

    let body = chat_body(model, &messages, thinking, 8_192);

    let response = client
        .post(format!("{base_url}/api/chat"))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            ResearchError::Network(format!(
                "could not reach the local Ollama server at {base_url}; is it running? ({e})"
            ))
        })?;

    if !response.status().is_success() {
        return Err(server_error(response).await);
    }

    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    while let Some(bytes) = stream.next().await {
        let bytes = bytes.map_err(|e| ResearchError::Network(e.to_string()))?;
        buffer.extend_from_slice(&bytes);
        while let Some(newline) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let chunk: StreamChunk = match serde_json::from_str(line) {
                Ok(chunk) => chunk,
                Err(_) => continue,
            };
            let StreamChunk { message, done } = chunk;
            let StreamMessage { thinking, content } = message.unwrap_or_default();

            if let Some(thinking) = thinking {
                if !thinking.is_empty() {
                    out.thinking.push_str(&thinking);
                    if let Some(sink) = sink {
                        sink(StreamEvent::ThinkingDelta { delta: thinking });
                    }
                }
            }
            if let Some(content) = content {
                if !content.is_empty() {
                    if done && out.message.is_empty() {
                        out.message = content;
                    } else {
                        out.message.push_str(&content);
                        if let Some(sink) = sink {
                            sink(StreamEvent::AnswerDelta { delta: content });
                        }
                    }
                }
            }
        }
    }

    out.message = out.message.trim().to_string();
    Ok(())
}

/// Normalize a local server base URL: add a scheme and strip trailing slashes.
fn normalize_base(base: &str) -> Result<String, ResearchError> {
    let raw = base.trim().to_string();
    if raw.is_empty() {
        return Ok(DEFAULT_LOCAL_BASE.to_string());
    }
    let with_scheme = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw
    } else {
        format!("http://{raw}")
    };
    let parsed = url::Url::parse(&with_scheme)
        .map_err(|_| ResearchError::InvalidConfig(format!("invalid Ollama server address: {base}")))?;
    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

fn http_client() -> Result<Client, ResearchError> {
    Client::builder()
        .user_agent("research-assistant/0.1")
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| ResearchError::Network(e.to_string()))
}

/// List models installed on a local Ollama server (non-UniFFI helper).
async fn local_models_inner(base_url: &str) -> Result<Vec<ModelInfo>, ResearchError> {
    let client = http_client()?;
    let base = normalize_base(base_url)?;
    let response = client
        .get(format!("{base}/api/tags"))
        .send()
        .await
        .map_err(|e| ResearchError::Network(format!("could not reach {base}: {e}")))?;
    if !response.status().is_success() {
        return Err(server_error(response).await);
    }
    let payload: TagsResponse = response
        .json()
        .await
        .map_err(|e| ResearchError::InvalidResponse(e.to_string()))?;

    let mut models: Vec<ModelInfo> = payload
        .models
        .into_iter()
        .filter(|m| is_cloud_chat_model(&m.name))
        .map(|m| ModelInfo {
            endpoint: Endpoint::Local,
            name: m.name,
            parameter_size: m.details.parameter_size,
            capabilities: Vec::new(),
        })
        .collect();

    let capability_jobs: Vec<_> = models
        .iter()
        .map(|m| local_show(&client, &base, &m.name))
        .collect();
    let capability_lists = futures::future::join_all(capability_jobs).await;
    for (model, capabilities) in models.iter_mut().zip(capability_lists) {
        if let Ok(capabilities) = capabilities {
            model.capabilities = capabilities;
        }
    }
    Ok(models)
}

/// UniFFI-exported local model listing (free async function is supported).
#[uniffi::export]
pub async fn list_local_models(base_url: String) -> Result<Vec<ModelInfo>, ResearchError> {
    local_models_inner(&base_url).await
}

async fn local_show(
    client: &Client,
    base: &str,
    model: &str,
) -> Result<Vec<String>, ResearchError> {
    let response = client
        .post(format!("{base}/api/show"))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .map_err(|e| ResearchError::Network(e.to_string()))?;
    if !response.status().is_success() {
        return Err(server_error(response).await);
    }
    let payload: ShowResponse = response
        .json()
        .await
        .map_err(|e| ResearchError::InvalidResponse(e.to_string()))?;
    Ok(payload.capabilities)
}

fn emit(sink: &Option<StreamSink>, event: StreamEvent) {
    if let Some(sink) = sink {
        sink(event);
    }
}

fn supports_thinking(model: &CloudModel) -> bool {
    model
        .capabilities
        .iter()
        .any(|capability| capability == "thinking")
}
#[uniffi::export]
pub async fn web_search(
    query: String,
    max_results: u32,
    api_key: String,
) -> Result<Vec<SearchSource>, ResearchError> {
    let client = Client::new();
    let body = serde_json::json!({ "query": query, "max_results": max_results.min(10) });
    let response = client
        .post(format!("{API_BASE}/web_search"))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| ResearchError::Network(e.to_string()))?;

    if !response.status().is_success() {
        return Err(server_error(response).await);
    }

    let payload: WebSearchResponse = response
        .json()
        .await
        .map_err(|e| ResearchError::InvalidResponse(e.to_string()))?;

    Ok(payload
        .results
        .into_iter()
        .map(|r| SearchSource {
            title: r.title.unwrap_or_else(|| "Untitled source".to_string()),
            url: r.url.unwrap_or_default(),
            content: r.content.unwrap_or_default(),
        })
        .collect())
}

#[uniffi::export]
pub async fn web_fetch(url: String, api_key: String) -> Result<SearchSource, ResearchError> {
    let client = Client::new();
    let body = serde_json::json!({ "url": url });
    let response = client
        .post(format!("{API_BASE}/web_fetch"))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| ResearchError::Network(e.to_string()))?;

    if !response.status().is_success() {
        return Err(server_error(response).await);
    }

    let payload: WebFetchResponse = response
        .json()
        .await
        .map_err(|e| ResearchError::InvalidResponse(e.to_string()))?;

    Ok(SearchSource {
        title: payload.title.unwrap_or_else(|| "Untitled page".to_string()),
        url,
        content: payload.content.unwrap_or_default(),
    })
}

#[uniffi::export]
pub fn encrypt_settings(settings: AppSettings, device_salt: String) -> Result<String, ResearchError> {
    let key = derive_key(&device_salt);
    let cipher = ChaCha20Poly1305::new(&key);
    let mut nonce_bytes = [0u8; 12];
    getrandom::getrandom(&mut nonce_bytes).map_err(|e| ResearchError::Crypto(e.to_string()))?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let plaintext = serde_json::to_vec(&settings).map_err(|e| ResearchError::Crypto(e.to_string()))?;
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|e| ResearchError::Crypto(e.to_string()))?;

    let mut blob = nonce_bytes.to_vec();
    blob.extend_from_slice(&ciphertext);
    Ok(BASE64.encode(blob))
}

#[uniffi::export]
pub fn decrypt_settings(data: String, device_salt: String) -> Result<AppSettings, ResearchError> {
    let blob = BASE64.decode(data).map_err(|e| ResearchError::Crypto(e.to_string()))?;
    if blob.len() < 12 + 16 {
        return Err(ResearchError::Crypto("settings are corrupted".to_string()));
    }
    let nonce = Nonce::from_slice(&blob[..12]);
    let key = derive_key(&device_salt);
    let cipher = ChaCha20Poly1305::new(&key);
    let plaintext = cipher
        .decrypt(nonce, &blob[12..])
        .map_err(|_| ResearchError::Crypto("could not decrypt settings; re-enter your API key".to_string()))?;
    serde_json::from_slice(&plaintext).map_err(|e| ResearchError::Crypto(e.to_string()))
}

#[uniffi::export]
pub fn validate_research(query: &str, mode: &str) -> Result<(), ResearchError> {
    if query.trim().is_empty() {
        return Err(ResearchError::InvalidConfig(
            "the query cannot be empty".to_string(),
        ));
    }
    if !matches!(mode, "speed" | "balanced" | "quality") {
        return Err(ResearchError::InvalidConfig(
            "unknown research mode".to_string(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn derive_key(salt: &str) -> Key {
    let hash = Sha256::digest(format!("research-assistant::{salt}").as_bytes());
    *Key::from_slice(&hash)
}

fn is_cloud_chat_model(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && !n.contains("embed") && !n.contains("encoder")
}

/// Strip an optional `data:image/...;base64,` prefix and whitespace from
/// base64 image payloads; reject obviously bad data.
fn sanitize_images(images: Vec<String>) -> Vec<String> {
    images
        .into_iter()
        .map(|image| {
            let trimmed = image.trim();
            let payload = match trimmed.split_once(',') {
                Some((prefix, rest)) if prefix.starts_with("data:") => rest.trim(),
                _ => trimmed,
            };
            payload.chars().filter(|c| !c.is_whitespace()).collect::<String>()
        })
        .filter(|image| image.len() > 64)
        .collect()
}

/// Pick a model that can handle image input.
///
/// Preference order (all decisions are based on capabilities reported by
/// Ollama's `/api/show`; there is no name guessing, which would risk silently
/// sending images to a text-only model):
///   1. The primary model, if it advertises `vision`.
///   2. The user-configured fallback model, if it advertises `vision`.
///   3. The first known model advertising `vision`.
///
/// If nothing supports vision, a clear error is returned so the caller can
/// tell the user instead of dropping the images.
fn choose_vision_model(
    primary: &str,
    fallback: &Option<String>,
    known: &[CloudModel],
) -> Result<String, ResearchError> {
    let cap = |name: &str| -> Option<bool> {
        known
            .iter()
            .find(|m| m.name == name)
            .map(|m| m.supports_vision())
    };

    if cap(primary).unwrap_or(false) {
        return Ok(primary.to_string());
    }

    if let Some(fallback) = fallback {
        if !fallback.trim().is_empty() && cap(fallback).unwrap_or(false) {
            return Ok(fallback.clone());
        }
    }

    if let Some(model) = known.iter().find(|m| m.supports_vision()) {
        return Ok(model.name.clone());
    }

    Err(ResearchError::InvalidConfig(
        format!("the selected model ({primary}) does not support images and no vision-capable cloud model is available; choose a vision model in Settings")
    ))
}

async fn fetch_capabilities(client: &Client, api_key: &str, model: &str) -> Result<Vec<String>, ResearchError> {
    let response = client
        .post(format!("{API_BASE}/show"))
        .bearer_auth(api_key)
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .map_err(|e| ResearchError::Network(e.to_string()))?;

    if !response.status().is_success() {
        return Err(server_error(response).await);
    }

    let payload: ShowResponse = response
        .json()
        .await
        .map_err(|e| ResearchError::InvalidResponse(e.to_string()))?;
    Ok(payload.capabilities)
}

async fn synthesize_search_terms(
    client: &Client,
    api_key: &str,
    model: &str,
    query: &str,
    history: &[HistoryEntry],
) -> Vec<String> {
    let mut messages = Vec::new();
    if !history.is_empty() {
        for entry in history.iter().rev().take(6).rev() {
            let role = if entry.role == "assistant" { "assistant" } else { "user" };
            messages.push(serde_json::json!({"role": role, "content": entry.content}));
        }
    }
    messages.push(serde_json::json!({
        "role": "user",
        "content": format!(
            "Generate exactly 3 short web search phrases to research this question. \
             Return one per line, no numbering, no JSON, no quotes.\n\nQuestion: {query}"
        )
    }));

    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
        "options": { "temperature": 0.1, "num_predict": 128 }
    });

    let response = match client
        .post(format!("{API_BASE}/chat"))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(_) => return vec![query.to_string()],
    };

    let payload: ChatResponse = match response.json().await {
        Ok(p) => p,
        Err(_) => return vec![query.to_string()],
    };

    let text = payload.message.content;
    let terms: Vec<String> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.chars().take(120).collect())
        .collect();
    if terms.is_empty() { vec![query.to_string()] } else { terms }
}

/// Generate search phrases with a local model (no API key involved). Falls
/// back to the raw query when the local server is unreachable.
async fn synthesize_search_terms_local(
    base_url: &str,
    model: &str,
    query: &str,
    history: &[HistoryEntry],
) -> Vec<String> {
    let client = match http_client() {
        Ok(client) => client,
        Err(_) => return vec![query.to_string()],
    };

    let mut messages = Vec::new();
    if !history.is_empty() {
        for entry in history.iter().rev().take(6).rev() {
            let role = if entry.role == "assistant" { "assistant" } else { "user" };
            messages.push(serde_json::json!({"role": role, "content": entry.content}));
        }
    }
    messages.push(serde_json::json!({
        "role": "user",
        "content": format!(
            "Generate exactly 3 short web search phrases to research this question. \
             Return one per line, no numbering, no JSON, no quotes.\n\nQuestion: {query}"
        )
    }));

    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
        "options": { "temperature": 0.1, "num_predict": 128 }
    });

    let response = match client.post(format!("{base_url}/api/chat")).json(&body).send().await {
        Ok(resp) => resp,
        Err(_) => return vec![query.to_string()],
    };
    if !response.status().is_success() {
        return vec![query.to_string()];
    }

    let payload: ChatResponse = match response.json().await {
        Ok(p) => p,
        Err(_) => return vec![query.to_string()],
    };

    let text = payload.message.content;
    let terms: Vec<String> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.chars().take(120).collect())
        .collect();
    if terms.is_empty() { vec![query.to_string()] } else { terms }
}

fn build_context(sources: &[SearchSource], mode: &str) -> String {
    let limit = match mode {
        "speed" => 4_000,
        "quality" => 18_000,
        _ => 9_000,
    };
    let mut context = String::new();
    for (i, source) in sources.iter().enumerate() {
        let chunk = format!(
            "--- Source {} ---\nTitle: {}\nURL: {}\nContent: {}\n\n",
            i + 1,
            source.title,
            source.url,
            source.content.chars().take(limit / sources.len().max(1)).collect::<String>()
        );
        if context.len() + chunk.len() > limit {
            break;
        }
        context.push_str(&chunk);
    }
    context
}

#[allow(clippy::too_many_arguments)]
async fn chat_with_context(
    client: &Client,
    api_key: &str,
    model: &str,
    query: &str,
    context: &str,
    history: &[HistoryEntry],
    images: &[String],
    thinking: bool,
    sink: &Option<StreamSink>,
    out: &mut ResearchResponse,
) -> Result<(), ResearchError> {
    let system = if images.is_empty() {
        "You are a research assistant. Answer the user's question using the provided sources. \
         Be precise, cite sources with inline URLs in markdown, and highlight uncertainty. \
         If sources conflict, say so. Keep answers focused and complete."
    } else {
        "You are a research assistant with vision. Describe and analyse the attached images in the \
         context of the user's question. Reference the earlier conversation when relevant. \
         Keep answers focused and complete."
    };

    let mut messages = vec![serde_json::json!({"role": "system", "content": system})];
    for entry in history.iter().rev().take(10).rev() {
        let role = if entry.role == "assistant" { "assistant" } else { "user" };
        messages.push(serde_json::json!({"role": role, "content": entry.content}));
    }

    let content = if context.is_empty() {
        query.to_string()
    } else {
        format!("Question: {query}\n\nSources:\n{context}")
    };

    let mut user_message = serde_json::json!({"role": "user", "content": content});
    if !images.is_empty() {
        user_message["images"] = serde_json::json!(images);
    }
    messages.push(user_message);

    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "think": thinking,
        "options": { "temperature": 0.2, "num_ctx": 32_768 }
    });

    let response = client
        .post(format!("{API_BASE}/chat"))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| ResearchError::Network(e.to_string()))?;

    if response.status().as_u16() == 401 {
        return Err(ResearchError::Server(
            "invalid API key; check your key at ollama.com/settings/keys".to_string(),
        ));
    }
    if !response.status().is_success() {
        return Err(server_error(response).await);
    }

    // Newline-delimited JSON stream (`application/x-ndjson`). Each chunk may
    // carry `message.thinking` (reasoning trace), `message.content` (answer),
    // or a terminal `done` marker carrying the complete message.
    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    while let Some(bytes) = stream.next().await {
        let bytes = bytes.map_err(|e| ResearchError::Network(e.to_string()))?;
        buffer.extend_from_slice(&bytes);
        while let Some(newline) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let chunk: StreamChunk = match serde_json::from_str(line) {
                Ok(chunk) => chunk,
                Err(_) => continue,
            };
            let StreamChunk { message, done } = chunk;
            let StreamMessage { thinking, content } = message.unwrap_or_default();

            if let Some(thinking) = thinking {
                if !thinking.is_empty() {
                    out.thinking.push_str(&thinking);
                    if let Some(sink) = sink {
                        sink(StreamEvent::ThinkingDelta { delta: thinking });
                    }
                }
            }
            if let Some(content) = content {
                if !content.is_empty() {
                    if done && out.message.is_empty() {
                        // Terminal chunk carrying the whole buffered answer.
                        out.message = content;
                    } else {
                        out.message.push_str(&content);
                        if let Some(sink) = sink {
                            sink(StreamEvent::AnswerDelta { delta: content });
                        }
                    }
                }
            }
        }
    }

    out.message = out.message.trim().to_string();
    Ok(())
}

async fn server_error(response: reqwest::Response) -> ResearchError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .or_else(|| v.get("message"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| body.chars().take(240).collect());
    ResearchError::Server(format!("{}: {}", status.as_u16(), detail))
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
    #[serde(default)]
    details: TagDetails,
}

#[derive(Deserialize, Default)]
struct TagDetails {
    #[serde(default)]
    parameter_size: String,
}

#[derive(Deserialize)]
struct WebSearchResponse {
    results: Vec<WebSearchResult>,
}

#[derive(Deserialize)]
struct WebSearchResult {
    title: Option<String>,
    url: Option<String>,
    content: Option<String>,
}

#[derive(Deserialize)]
struct WebFetchResponse {
    title: Option<String>,
    content: Option<String>,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: String,
}

/// One NDJSON chunk from `/api/chat` when `stream` is enabled.
/// `message` carries incremental `thinking` / `content` deltas; the terminal
/// chunk (with `done: true`) may repeat the complete message.
#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    message: Option<StreamMessage>,
    #[serde(default)]
    done: bool,
}

#[derive(Deserialize, Default)]
struct StreamMessage {
    thinking: Option<String>,
    content: Option<String>,
}

#[derive(Deserialize)]
struct ShowResponse {
    #[serde(default)]
    capabilities: Vec<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> AppSettings {
        AppSettings {
            api_key: "test-key".to_string(),
            model: "gpt-oss:120b".to_string(),
            vision_model: None,
            mode: "balanced".to_string(),
            local_base_url: String::new(),
            local_enabled: false,
        }
    }

    #[test]
    fn encrypts_and_decrypts_settings() {
        let encoded = encrypt_settings(settings(), "device-salt".to_string()).unwrap();
        let decoded = decrypt_settings(encoded, "device-salt".to_string()).unwrap();
        assert_eq!(decoded.api_key, "test-key");
        assert_eq!(decoded.model, "gpt-oss:120b");
    }

    #[test]
    fn rejects_wrong_salt() {
        let encoded = encrypt_settings(settings(), "salt-a".to_string()).unwrap();
        assert!(decrypt_settings(encoded, "salt-b".to_string()).is_err());
    }

    #[test]
    fn validates_research() {
        assert!(validate_research("  ", "balanced").is_err());
        assert!(validate_research("What is Ollama?", "quality").is_ok());
        assert!(validate_research("What is Ollama?", "unknown").is_err());
    }

    #[test]
    fn filters_cloud_chat_models() {
        assert!(is_cloud_chat_model("gpt-oss:120b-cloud"));
        assert!(is_cloud_chat_model("qwen3:4b-cloud"));
        assert!(!is_cloud_chat_model("nomic-embed-text"));
        assert!(!is_cloud_chat_model(""));
    }

    fn cloud_model(name: &str, capabilities: &[&str]) -> CloudModel {
        CloudModel {
            name: name.to_string(),
            parameter_size: String::new(),
            capabilities: capabilities.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn keeps_primary_model_when_it_supports_vision() {
        let known = vec![cloud_model("qwen3-vl:235b-cloud", &["completion", "vision"])];
        let chosen =
            choose_vision_model("qwen3-vl:235b-cloud", &None, &known).unwrap();
        assert_eq!(chosen, "qwen3-vl:235b-cloud");
    }

    #[test]
    fn routes_images_to_vision_model_when_primary_lacks_vision() {
        let known = vec![
            cloud_model("gpt-oss:120b-cloud", &["completion", "tools"]),
            cloud_model("qwen3-vl:235b-cloud", &["completion", "vision"]),
        ];
        let chosen = choose_vision_model("gpt-oss:120b-cloud", &None, &known).unwrap();
        assert_eq!(chosen, "qwen3-vl:235b-cloud");
    }

    #[test]
    fn prefers_user_configured_vision_fallback() {
        let known = vec![
            cloud_model("gpt-oss:120b-cloud", &["completion", "tools"]),
            cloud_model("qwen3-vl:235b-cloud", &["completion", "vision"]),
        ];
        let chosen = choose_vision_model(
            "gpt-oss:120b-cloud",
            &Some("qwen3-vl:235b-cloud".to_string()),
            &known,
        )
        .unwrap();
        assert_eq!(chosen, "qwen3-vl:235b-cloud");
    }

    #[test]
    fn errors_when_no_vision_model_available() {
        let known = vec![cloud_model("gpt-oss:120b-cloud", &["completion"])];
        assert!(choose_vision_model("gpt-oss:120b-cloud", &None, &known).is_err());
    }

    #[test]
    fn strips_data_url_prefix_from_images() {
        let long = "a".repeat(100);
        let images = sanitize_images(vec![format!("data:image/png;base64,{long}")]);
        assert_eq!(images, vec![long]);
    }

    #[test]
    fn encodes_and_decodes_model_ids() {
        assert_eq!(encode_model_id(Endpoint::Cloud, "gpt-oss:120b"), "cloud:gpt-oss:120b");
        assert_eq!(encode_model_id(Endpoint::Local, "llama3.2:3b"), "local:llama3.2:3b");
        assert_eq!(decode_model_id("local:llama3.2:3b"), (Endpoint::Local, "llama3.2:3b".to_string()));
        assert_eq!(decode_model_id("cloud:gpt-oss:120b"), (Endpoint::Cloud, "gpt-oss:120b".to_string()));
        // Bare names keep working as cloud models.
        assert_eq!(decode_model_id("gpt-oss:120b-cloud"), (Endpoint::Cloud, "gpt-oss:120b-cloud".to_string()));
    }

    #[test]
    fn normalizes_local_base_urls() {
        assert_eq!(normalize_base("").unwrap(), DEFAULT_LOCAL_BASE);
        assert_eq!(normalize_base("192.168.1.10:11434").unwrap(), "http://192.168.1.10:11434");
        assert_eq!(normalize_base("http://localhost:11434").unwrap(), "http://localhost:11434");
        assert_eq!(normalize_base("http://localhost:11434/").unwrap(), "http://localhost:11434");
        assert!(normalize_base("not a url").is_err());
    }

    fn model_info(endpoint: Endpoint, name: &str, capabilities: &[&str]) -> ModelInfo {
        ModelInfo {
            endpoint,
            name: name.to_string(),
            parameter_size: String::new(),
            capabilities: capabilities.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn routes_images_across_endpoints() {
        let mut settings = settings();
        settings.local_enabled = true;
        let known = vec![
            model_info(Endpoint::Cloud, "gpt-oss:120b", &["completion"]),
            model_info(Endpoint::Local, "llava:7b", &["vision"]),
        ];

        // Images with a text-only primary cloud model fall back to local vision.
        let resolved = resolve_turn_model(Endpoint::Cloud, "gpt-oss:120b", &settings, &["a".repeat(200)], &known).unwrap();
        assert_eq!(resolved.endpoint, Endpoint::Local);
        assert_eq!(resolved.model_name, "llava:7b");

        // Text turns stay on the primary model.
        let resolved = resolve_turn_model(Endpoint::Cloud, "gpt-oss:120b", &settings, &[], &known).unwrap();
        assert_eq!(resolved.endpoint, Endpoint::Cloud);
        assert_eq!(resolved.model_name, "gpt-oss:120b");
    }

    #[test]
    fn local_turn_needs_local_enabled_for_cross_routing() {
        let mut settings = settings();
        settings.local_enabled = false;
        let known = vec![model_info(Endpoint::Local, "llava:7b", &["vision"])];
        assert!(resolve_turn_model(Endpoint::Cloud, "gpt-oss:120b", &settings, &["a".repeat(200)], &known).is_err());
    }

    #[test]
    fn local_primary_uses_local_vision() {
        let settings = settings();
        let known = vec![
            model_info(Endpoint::Local, "llava:7b", &["vision"]),
            model_info(Endpoint::Local, "mistral:7b", &["completion"]),
        ];
        let resolved = resolve_turn_model(Endpoint::Local, "mistral:7b", &settings, &["a".repeat(200)], &known).unwrap();
        assert_eq!(resolved.endpoint, Endpoint::Local);
        assert_eq!(resolved.model_name, "llava:7b");
    }

    #[test]
    fn local_settings_round_trip() {
        let mut value = settings();
        value.local_base_url = "http://192.168.1.5:11434".to_string();
        value.local_enabled = true;
        let encoded = encrypt_settings(value, "salt".to_string()).unwrap();
        let decoded = decrypt_settings(encoded, "salt".to_string()).unwrap();
        assert_eq!(decoded.local_base_url, "http://192.168.1.5:11434");
        assert!(decoded.local_enabled);
    }
}
