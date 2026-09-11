use serde::Serialize;
use std::fs;
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager};
use vane_core::{
    AppSettings, CloudModel, Endpoint, EncodedSettings, HistoryEntry, ModelInfo, ResearchClient,
    ResearchResponse, StreamEvent,
};

#[derive(Serialize)]
struct SettingsStatus {
    configured: bool,
    model: String,
    vision_model: Option<String>,
    mode: String,
    local_base_url: String,
    local_enabled: bool,
    has_api_key: bool,
}

fn status_from(settings: Option<AppSettings>) -> SettingsStatus {
    match settings {
        Some(settings) => SettingsStatus {
            configured: !settings.api_key.is_empty() || settings.local_enabled,
            model: settings.model,
            vision_model: settings.vision_model,
            mode: settings.mode,
            local_base_url: settings.local_base_url,
            local_enabled: settings.local_enabled,
            has_api_key: !settings.api_key.is_empty(),
        },
        None => SettingsStatus {
            configured: false,
            model: String::new(),
            vision_model: None,
            mode: String::new(),
            local_base_url: String::new(),
            local_enabled: false,
            has_api_key: false,
        },
    }
}

#[tauri::command]
fn get_settings_status(app: AppHandle) -> Result<SettingsStatus, String> {
    let app_settings = load_settings(&app)?;
    Ok(status_from(app_settings))
}

/// Validate the API key, list available cloud models (with capabilities), and
/// store the key encrypted until a model is chosen.
#[tauri::command]
async fn setup(app: AppHandle, api_key: String) -> Result<Vec<CloudModel>, String> {
    let client = ResearchClient::new(api_key.clone(), "setup".to_string())
        .map_err(|e| e.to_string())?;
    let models = client.models().await.map_err(|e| e.to_string())?;

    let settings = AppSettings {
        api_key,
        model: String::new(),
        vision_model: None,
        mode: "balanced".to_string(),
        local_base_url: String::new(),
        local_enabled: false,
    };
    store_settings(&app, &settings)?;
    Ok(models)
}

/// Probe a local Ollama server and store its address (and enabled flag).
#[tauri::command]
async fn setup_local(app: AppHandle, base_url: String) -> Result<Vec<ModelInfo>, String> {
    let models = vane_core::list_local_models(base_url.clone())
        .await
        .map_err(|e| e.to_string())?;
    if models.is_empty() {
        return Err("No models are installed on that Ollama server (run `ollama pull <model>`).".to_string());
    }

    let mut settings = load_settings(&app)?.unwrap_or_else(|| AppSettings {
        api_key: String::new(),
        model: String::new(),
        vision_model: None,
        mode: "balanced".to_string(),
        local_base_url: String::new(),
        local_enabled: false,
    });
    settings.local_base_url = base_url;
    settings.local_enabled = true;
    if settings.mode.is_empty() {
        settings.mode = "balanced".to_string();
    }
    store_settings(&app, &settings)?;
    Ok(models)
}

/// Store only the API key (used when re-adding cloud access later).
#[tauri::command]
async fn save_api_key(app: AppHandle, api_key: String) -> Result<Vec<CloudModel>, String> {
    let client = ResearchClient::new(api_key.clone(), "setup".to_string())
        .map_err(|e| e.to_string())?;
    let models = client.models().await.map_err(|e| e.to_string())?;

    let mut settings = load_settings(&app)?.unwrap_or_else(|| AppSettings {
        api_key: String::new(),
        model: String::new(),
        vision_model: None,
        mode: "balanced".to_string(),
        local_base_url: String::new(),
        local_enabled: false,
    });
    settings.api_key = api_key;
    if settings.mode.is_empty() {
        settings.mode = "balanced".to_string();
    }
    store_settings(&app, &settings)?;
    Ok(models)
}

/// Persist the chosen chat model, optional vision fallback, and research mode.
#[tauri::command]
async fn save_model(
    app: AppHandle,
    model: String,
    vision_model: Option<String>,
    mode: String,
) -> Result<(), String> {
    let mut settings = load_settings(&app)?
        .ok_or_else(|| "Complete setup first.".to_string())?;
    settings.model = model;
    settings.vision_model = vision_model;
    settings.mode = mode;
    store_settings(&app, &settings)
}

/// Update the local Ollama server address / enabled flag without re-probing.
#[tauri::command]
async fn save_local(
    app: AppHandle,
    base_url: String,
    enabled: bool,
) -> Result<Vec<ModelInfo>, String> {
    let mut settings = load_settings(&app)?
        .ok_or_else(|| "Complete setup first.".to_string())?;

    let mut models = Vec::new();
    if enabled {
        models = vane_core::list_local_models(base_url.clone())
            .await
            .map_err(|e| e.to_string())?;
    }
    settings.local_base_url = base_url;
    settings.local_enabled = enabled;
    store_settings(&app, &settings)?;
    Ok(models)
}

/// Refresh the cloud model list (e.g. after a key rotation).
#[tauri::command]
async fn list_models(app: AppHandle) -> Result<Vec<CloudModel>, String> {
    let settings = load_settings(&app)?
        .ok_or("Complete setup first.")?;
    let client = ResearchClient::new(settings.api_key, "list".to_string())
        .map_err(|e| e.to_string())?;
    client.models().await.map_err(|e| e.to_string())
}

/// Refresh the local model list from the configured Ollama server.
#[tauri::command]
async fn list_local_models(app: AppHandle) -> Result<Vec<ModelInfo>, String> {
    let settings = load_settings(&app)?.ok_or("Complete setup first.")?;
    vane_core::list_local_models(settings.local_base_url)
        .await
        .map_err(|e| e.to_string())
}

/// Combined model list (cloud + local) used for the model switcher.
#[tauri::command]
async fn list_all_models(app: AppHandle) -> Result<Vec<ModelInfo>, String> {
    let settings = load_settings(&app)?.ok_or("Complete setup first.")?;
    let mut models: Vec<ModelInfo> = Vec::new();
    if !settings.api_key.is_empty() {
        match ResearchClient::new(settings.api_key.clone(), "list".to_string())
            .map_err(|e| e.to_string())?
            .models()
            .await
        {
            Ok(cloud) => {
                models.extend(cloud.into_iter().map(|m| ModelInfo {
                    endpoint: Endpoint::Cloud,
                    name: m.name,
                    parameter_size: m.parameter_size,
                    capabilities: m.capabilities,
                }));
            }
            // Cloud being unreachable must not hide local models.
            Err(_) => {}
        }
    }
    if settings.local_enabled && !settings.local_base_url.trim().is_empty() {
        match vane_core::list_local_models(settings.local_base_url).await {
            Ok(local) => models.extend(local),
            Err(_) => {}
        }
    }
    Ok(models)
}

#[tauri::command]
async fn research(
    app: AppHandle,
    query: String,
    history: Vec<HistoryEntry>,
    images: Vec<String>,
    on_event: Channel<StreamEvent>,
) -> Result<ResearchResponse, String> {
    let settings = load_settings(&app)?
        .ok_or("Complete setup first.")?;
    if settings.model.is_empty() {
        return Err("Choose a model in Settings first.".to_string());
    }
    let client = ResearchClient::for_shell(&settings).map_err(|e| e.to_string())?;
    let mode = settings.mode.clone();

    // Refresh the known model list so capability-driven routing (not name
    // heuristics) decides where image turns go.
    let mut known_models: Vec<ModelInfo> = Vec::new();
    if !settings.api_key.is_empty() {
        if let Ok(cloud) = ResearchClient::new(settings.api_key.clone(), "list".to_string())
            .map_err(|e| e.to_string())?
            .models()
            .await
        {
            known_models.extend(cloud.into_iter().map(|m| ModelInfo {
                endpoint: Endpoint::Cloud,
                name: m.name,
                parameter_size: m.parameter_size,
                capabilities: m.capabilities,
            }));
        }
    }
    if settings.local_enabled && !settings.local_base_url.trim().is_empty() {
        if let Ok(local) = vane_core::list_local_models(settings.local_base_url.clone()).await {
            known_models.extend(local);
        }
    }

    let sink = Arc::new(move |event: StreamEvent| {
        let _ = on_event.send(event);
    });

    client
        .research_any(
            &settings,
            query,
            mode,
            history,
            images,
            known_models,
            Some(sink),
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn reset_settings(app: AppHandle) -> Result<(), String> {
    let settings = settings_path(&app)?;
    if settings.exists() {
        fs::remove_file(settings).map_err(|e| e.to_string())?;
    }
    // Also rotate the salt so any leftover ciphertext is undecryptable.
    let salt = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("key.salt");
    if salt.exists() {
        fs::remove_file(salt).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn load_settings(app: &AppHandle) -> Result<Option<AppSettings>, String> {
    let path = settings_path(app)?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let encoded: EncodedSettings = serde_json::from_str(&contents)
        .map_err(|e| format!("Could not read settings: {e}"))?;
    let salt = device_salt(app)?;
    let settings = vane_core::decrypt_settings(encoded.data, salt).map_err(|e| e.to_string())?;
    Ok(Some(settings))
}

fn store_settings(app: &AppHandle, settings: &AppSettings) -> Result<(), String> {
    let salt = device_salt(app)?;
    let data = vane_core::encrypt_settings(settings.clone(), salt).map_err(|e| e.to_string())?;
    let encoded = EncodedSettings { data };
    let path = settings_path(app)?;
    let parent = path.parent().ok_or("Could not determine settings directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    fs::write(path, serde_json::to_string_pretty(&encoded).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

fn settings_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|d| d.join("settings.json"))
        .map_err(|e| e.to_string())
}

/// Stable per-device salt used to derive the settings encryption key.
/// Generated once as 32 random bytes (hex-encoded) and persisted next to the
/// settings file inside the platform app-config directory, which the OS
/// restricts to the current user account.
fn device_salt(app: &AppHandle) -> Result<String, String> {
    let config_dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    let path = config_dir.join("key.salt");

    if let Ok(salt) = fs::read_to_string(&path) {
        let salt = salt.trim().to_string();
        if salt.len() == 64 && salt.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(salt);
        }
    }

    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
    let salt: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    fs::write(&path, &salt).map_err(|e| e.to_string())?;
    Ok(salt)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_settings_status,
            setup,
            setup_local,
            save_api_key,
            save_model,
            save_local,
            list_models,
            list_local_models,
            list_all_models,
            research,
            reset_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Research Assistant");
}
