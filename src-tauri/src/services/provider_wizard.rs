//! Safe provider onboarding primitives.
//!
//! The probe phase is read-only; preview and apply intentionally persist the
//! selected provider through the normal provider services.

use crate::proxy::http_client;
use crate::services::model_fetch::build_models_url_candidates;
use crate::{app_config::AppType, codex_config, config};
use crate::{
    provider::{
        ClaudeDesktopMode, ClaudeDesktopModelRoute, OpenCodeModel, OpenCodeProviderConfig,
        OpenCodeProviderOptions, Provider, ProviderMeta,
    },
    store::AppState,
};
use once_cell::sync::Lazy;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONNECTION};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::Mutex;
use toml_edit::{value, DocumentMut, Item, Table};
use url::Url;

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_MODELS_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_DISCOVERED_MODELS: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamProtocol {
    AnthropicMessages,
    OpenAiChat,
    OpenAiResponses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    Bearer,
    XApiKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UrlMode {
    Base,
    FullEndpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProbeInput {
    pub base_url: String,
    pub api_key: String,
    pub models_url: Option<String>,
    pub model: Option<String>,
    pub allow_inference_probe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedModel {
    pub id: String,
    pub owned_by: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolCapability {
    pub protocol: UpstreamProtocol,
    pub endpoint: String,
    pub auth_mode: AuthMode,
    pub supported: bool,
    pub confidence: ProbeConfidence,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProbeResult {
    pub normalized_base_url: String,
    pub url_mode: UrlMode,
    pub models: Vec<DetectedModel>,
    pub capabilities: Vec<ProtocolCapability>,
    pub recommended_model: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstallSelection {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub models: Vec<DetectedModel>,
    pub claude_protocol: Option<UpstreamProtocol>,
    pub codex_protocol: Option<UpstreamProtocol>,
    pub claude_desktop_protocol: Option<UpstreamProtocol>,
    pub opencode_protocol: Option<UpstreamProtocol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInstallPreview {
    pub app: AppType,
    pub provider_id: String,
    pub protocol: UpstreamProtocol,
    pub mode: String,
    pub model: String,
    pub files_to_change: Vec<String>,
    pub restart_required: bool,
    pub redacted_config: Value,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstallPreview {
    pub provider_id: String,
    pub normalized_base_url: String,
    pub url_mode: UrlMode,
    pub claude: Option<AppInstallPreview>,
    pub codex: Option<AppInstallPreview>,
    pub claude_desktop: Option<AppInstallPreview>,
    pub opencode: Option<AppInstallPreview>,
    pub proxy_will_start: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyProviderInstallResult {
    pub applied_apps: Vec<AppType>,
    pub rolled_back: bool,
    pub rollback_errors: Vec<String>,
    pub restart_required_apps: Vec<AppType>,
}

static INSTALL_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[derive(Debug, Clone, Copy)]
struct EndpointCandidate {
    protocol: UpstreamProtocol,
    path: &'static str,
    preferred_auth: AuthMode,
}

const ENDPOINTS: [EndpointCandidate; 3] = [
    EndpointCandidate {
        protocol: UpstreamProtocol::AnthropicMessages,
        path: "/v1/messages",
        preferred_auth: AuthMode::XApiKey,
    },
    EndpointCandidate {
        protocol: UpstreamProtocol::OpenAiResponses,
        path: "/v1/responses",
        preferred_auth: AuthMode::Bearer,
    },
    EndpointCandidate {
        protocol: UpstreamProtocol::OpenAiChat,
        path: "/v1/chat/completions",
        preferred_auth: AuthMode::Bearer,
    },
];

pub async fn probe_provider_capabilities(
    input: ProviderProbeInput,
) -> Result<ProviderProbeResult, String> {
    if input.api_key.trim().is_empty() {
        return Err("API key is required".to_string());
    }

    let (normalized_base_url, url_mode, detected_protocol) = normalize_base_url(&input.base_url)?;
    let models = fetch_models_for_probe(
        &normalized_base_url,
        url_mode,
        input.models_url.as_deref(),
        &input.api_key,
    )
    .await?;

    let mut warnings = Vec::new();
    let requested_model = input
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty());
    let selected_model = if models.is_empty() {
        requested_model.map(str::to_string)
    } else {
        requested_model
            .and_then(|requested| {
                models
                    .iter()
                    .find(|model| model.id == requested)
                    .map(|model| model.id.clone())
            })
            .or_else(|| models.first().map(|model| model.id.clone()))
    };
    if requested_model.is_some()
        && selected_model.as_deref() != requested_model
        && !models.is_empty()
    {
        warnings.push(
            "The previously selected model is not in this provider catalog; probing with the first discovered model instead."
                .to_string(),
        );
    }
    if selected_model.is_none() {
        warnings.push("No model was discovered; enter a model manually.".to_string());
    }

    if !input.allow_inference_probe {
        warnings.push(
            "Inference probing was not authorized; protocol capabilities were not tested."
                .to_string(),
        );
        return Ok(ProviderProbeResult {
            normalized_base_url,
            url_mode,
            models,
            capabilities: detected_protocol
                .map(|candidate| ProtocolCapability {
                    protocol: candidate.protocol,
                    endpoint: candidate.path.to_string(),
                    auth_mode: candidate.preferred_auth,
                    supported: false,
                    confidence: ProbeConfidence::Low,
                    evidence: vec!["Endpoint inferred from the supplied URL.".to_string()],
                })
                .into_iter()
                .collect(),
            recommended_model: selected_model,
            warnings,
        });
    }

    let Some(model) = selected_model.clone() else {
        return Err("A model is required before inference probing".to_string());
    };

    let client = http_client::get();
    let mut capabilities = Vec::new();
    for candidate in ENDPOINTS {
        if url_mode == UrlMode::FullEndpoint
            && detected_protocol.is_some_and(|detected| detected.protocol != candidate.protocol)
        {
            continue;
        }

        let endpoint = endpoint_url(&normalized_base_url, url_mode, candidate.path)?;
        let capability =
            probe_endpoint(&client, &endpoint, candidate, &input.api_key, &model).await;
        capabilities.push(capability);
    }

    if capabilities.iter().all(|capability| !capability.supported) {
        warnings.push("No protocol probe succeeded; review URL, auth mode, and model.".to_string());
    }

    Ok(ProviderProbeResult {
        normalized_base_url,
        url_mode,
        models,
        capabilities,
        recommended_model: Some(model),
        warnings,
    })
}

pub fn preview_provider_install(
    selection: ProviderInstallSelection,
) -> Result<ProviderInstallPreview, String> {
    let name = selection.name.trim();
    if name.is_empty() {
        return Err("Provider name is required".to_string());
    }
    if selection.api_key.trim().is_empty() {
        return Err("API key is required".to_string());
    }
    let model = selection.model.trim();
    if model.is_empty() {
        return Err("Model is required".to_string());
    }

    let (normalized_base_url, url_mode, _) = normalize_base_url(&selection.base_url)?;
    let provider_id = wizard_provider_id(name, &normalized_base_url);
    let mut warnings = Vec::new();
    let claude = selection
        .claude_protocol
        .map(|protocol| build_claude_preview(&selection, &provider_id, protocol, model));
    let codex = selection
        .codex_protocol
        .map(|protocol| build_codex_preview(&selection, &provider_id, protocol, model));
    let claude_desktop = selection
        .claude_desktop_protocol
        .map(|protocol| build_claude_desktop_preview(&selection, &provider_id, protocol, model));
    let opencode = selection
        .opencode_protocol
        .map(|protocol| build_opencode_preview(&selection, &provider_id, protocol, model))
        .transpose()?;

    if claude.is_none() && codex.is_none() && claude_desktop.is_none() && opencode.is_none() {
        warnings.push("Select at least one application to configure.".to_string());
    }
    let proxy_will_start = claude
        .as_ref()
        .is_some_and(|preview| preview.mode == "proxy")
        || codex
            .as_ref()
            .is_some_and(|preview| preview.mode == "proxy")
        || claude_desktop
            .as_ref()
            .is_some_and(|preview| preview.mode == "proxy");

    Ok(ProviderInstallPreview {
        provider_id,
        normalized_base_url,
        url_mode,
        claude,
        codex,
        claude_desktop,
        opencode,
        proxy_will_start,
        warnings,
    })
}

pub async fn apply_provider_install(
    state: &AppState,
    selection: ProviderInstallSelection,
) -> Result<ApplyProviderInstallResult, String> {
    let _lock = INSTALL_LOCK.lock().await;
    let preview = preview_provider_install(selection.clone())?;
    let mut snapshots = Vec::new();
    let mut applied_apps = Vec::new();
    let mut restart_required_apps = Vec::new();

    for (app, protocol) in [
        (AppType::Claude, selection.claude_protocol),
        (AppType::Codex, selection.codex_protocol),
        (AppType::ClaudeDesktop, selection.claude_desktop_protocol),
        (AppType::OpenCode, selection.opencode_protocol),
    ] {
        let Some(protocol) = protocol else {
            continue;
        };
        let provider =
            build_provider_for_app(&selection, &preview.provider_id, app.clone(), protocol)?;
        let previous_provider = state
            .db
            .get_provider_by_id(&provider.id, app.as_str())
            .map_err(|error| error.to_string())?;
        let previous_current = crate::settings::get_current_provider(&app);
        let previous_takeover = state
            .db
            .get_proxy_config_for_app(app.as_str())
            .await
            .map_err(|error| error.to_string())?
            .enabled;
        snapshots.push(InstallSnapshot {
            app: app.clone(),
            provider_id: provider.id.clone(),
            previous_provider,
            previous_current,
            previous_takeover,
        });

        let app_proxy = match app {
            AppType::Claude => preview
                .claude
                .as_ref()
                .is_some_and(|item| item.mode == "proxy"),
            AppType::Codex => preview
                .codex
                .as_ref()
                .is_some_and(|item| item.mode == "proxy"),
            AppType::ClaudeDesktop => preview
                .claude_desktop
                .as_ref()
                .is_some_and(|item| item.mode == "proxy"),
            AppType::OpenCode => preview
                .opencode
                .as_ref()
                .is_some_and(|item| item.mode == "proxy"),
            _ => false,
        };
        if let Err(error) = apply_one_app(state, app.clone(), provider, app_proxy).await {
            let rollback_errors = rollback_install(state, &snapshots).await;
            return Err(format_apply_error(error, rollback_errors));
        }
        applied_apps.push(app.clone());
        if app == AppType::Codex || app_proxy {
            restart_required_apps.push(app);
        }
    }

    Ok(ApplyProviderInstallResult {
        applied_apps,
        rolled_back: false,
        rollback_errors: Vec::new(),
        restart_required_apps,
    })
}

#[derive(Debug)]
struct InstallSnapshot {
    app: AppType,
    provider_id: String,
    previous_provider: Option<Provider>,
    previous_current: Option<String>,
    previous_takeover: bool,
}

async fn apply_one_app(
    state: &AppState,
    app: AppType,
    provider: Provider,
    proxy_will_start: bool,
) -> Result<(), String> {
    // Cowork writes its gateway URL while adding the provider, so an ephemeral
    // local port must be resolved before the Claude Desktop profile is written.
    if proxy_will_start && app == AppType::ClaudeDesktop {
        state.proxy_service.start().await?;
    }
    crate::services::provider::ProviderService::add(state, app.clone(), provider.clone(), true)
        .map_err(|error| error.to_string())?;

    if proxy_will_start && app == AppType::ClaudeDesktop {
        let mut config = state
            .db
            .get_proxy_config_for_app(app.as_str())
            .await
            .map_err(|error| error.to_string())?;
        config.enabled = true;
        state
            .db
            .update_proxy_config_for_app(config)
            .await
            .map_err(|error| error.to_string())?;
    }

    if proxy_will_start && app.supports_local_proxy() {
        state
            .proxy_service
            .set_takeover_for_app(app.as_str(), true)
            .await?;
    }

    if app.is_additive_mode() {
        if proxy_will_start && app == AppType::OpenCode {
            state
                .proxy_service
                .enable_opencode_gateway(&provider.id)
                .await?;
        }
        return Ok(());
    }

    crate::services::provider::ProviderService::switch(state, app, &provider.id)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

async fn rollback_install(state: &AppState, snapshots: &[InstallSnapshot]) -> Vec<String> {
    let mut errors = Vec::new();
    for snapshot in snapshots.iter().rev() {
        if let Err(error) = rollback_one_app(state, snapshot).await {
            errors.push(format!("{}: {error}", snapshot.app.as_str()));
        }
    }
    errors
}

async fn rollback_one_app(state: &AppState, snapshot: &InstallSnapshot) -> Result<(), String> {
    if snapshot.app == AppType::OpenCode && !snapshot.previous_takeover {
        state.proxy_service.disable_opencode_gateway().await?;
    }
    if snapshot.previous_takeover && snapshot.app != AppType::OpenCode {
        state
            .proxy_service
            .set_takeover_for_app(snapshot.app.as_str(), true)
            .await?;
    } else if snapshot.app.supports_local_proxy() {
        state
            .proxy_service
            .set_takeover_for_app(snapshot.app.as_str(), false)
            .await?;
    }
    if snapshot.app == AppType::ClaudeDesktop {
        let mut config = state
            .db
            .get_proxy_config_for_app(snapshot.app.as_str())
            .await
            .map_err(|error| error.to_string())?;
        config.enabled = snapshot.previous_takeover;
        state
            .db
            .update_proxy_config_for_app(config)
            .await
            .map_err(|error| error.to_string())?;
    }

    if let Some(previous_provider) = &snapshot.previous_provider {
        state
            .db
            .save_provider(snapshot.app.as_str(), previous_provider)
            .map_err(|error| error.to_string())?;
        if snapshot.app.is_additive_mode() {
            crate::services::provider::ProviderService::update(
                state,
                snapshot.app.clone(),
                None,
                previous_provider.clone(),
            )
            .map_err(|error| error.to_string())?;
        }
        if let Some(previous_current) = &snapshot.previous_current {
            crate::settings::set_current_provider(&snapshot.app, Some(previous_current))
                .map_err(|error| error.to_string())?;
            state
                .db
                .set_current_provider(snapshot.app.as_str(), previous_current)
                .map_err(|error| error.to_string())?;
            crate::services::provider::ProviderService::switch(
                state,
                snapshot.app.clone(),
                previous_current,
            )
            .map_err(|error| error.to_string())?;
            if snapshot.app == AppType::OpenCode && snapshot.previous_takeover {
                state
                    .proxy_service
                    .enable_opencode_gateway(previous_current)
                    .await?;
            }
        }
    } else {
        if snapshot.app.is_additive_mode() {
            crate::services::provider::ProviderService::remove_from_live_config(
                state,
                snapshot.app.clone(),
                &snapshot.provider_id,
            )
            .map_err(|error| error.to_string())?;
        }
        state
            .db
            .delete_provider(snapshot.app.as_str(), &snapshot.provider_id)
            .map_err(|error| error.to_string())?;
        crate::settings::set_current_provider(&snapshot.app, None)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn format_apply_error(error: String, rollback_errors: Vec<String>) -> String {
    if rollback_errors.is_empty() {
        format!("Provider setup failed and was rolled back: {error}")
    } else {
        format!(
            "Provider setup failed: {error}; rollback also failed: {}",
            rollback_errors.join("; ")
        )
    }
}

fn build_provider_for_app(
    selection: &ProviderInstallSelection,
    provider_id: &str,
    app: AppType,
    protocol: UpstreamProtocol,
) -> Result<Provider, String> {
    let (base_url, url_mode, _) = normalize_base_url(&selection.base_url)?;
    let model = selection.model.trim();
    let mut provider = match app {
        AppType::Claude => {
            let auth_field = if protocol == UpstreamProtocol::AnthropicMessages {
                "ANTHROPIC_API_KEY"
            } else {
                "ANTHROPIC_AUTH_TOKEN"
            };
            let settings = serde_json::json!({
                "env": {
                    "ANTHROPIC_BASE_URL": base_url,
                    auth_field: selection.api_key.clone(),
                    "ANTHROPIC_MODEL": model,
                }
            });
            Provider::with_id(
                provider_id.to_string(),
                selection.name.clone(),
                settings,
                None,
            )
        }
        AppType::Codex => {
            let config_text =
                build_codex_config_text(provider_id, selection.name.trim(), &base_url, model);
            let settings = serde_json::json!({
                "auth": {"OPENAI_API_KEY": selection.api_key.clone()},
                "config": config_text,
                "modelCatalog": build_codex_model_catalog(selection),
            });
            Provider::with_id(
                provider_id.to_string(),
                selection.name.clone(),
                settings,
                None,
            )
        }
        AppType::ClaudeDesktop => {
            let models = normalized_install_models(selection);
            let uses_proxy = protocol != UpstreamProtocol::AnthropicMessages
                || models.iter().any(|detected| {
                    !crate::claude_desktop_config::is_claude_safe_model_id(&detected.id)
                });
            let routes = build_claude_desktop_model_routes(&models, uses_proxy);
            let settings = serde_json::json!({
                "env": {
                    "ANTHROPIC_BASE_URL": base_url,
                    "ANTHROPIC_AUTH_TOKEN": selection.api_key.clone(),
                    "ANTHROPIC_MODEL": model,
                }
            });
            let mut provider = Provider::with_id(
                provider_id.to_string(),
                selection.name.clone(),
                settings,
                None,
            );
            provider.meta = Some(ProviderMeta {
                api_format: Some(protocol_string(protocol).to_string()),
                api_key_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                claude_desktop_mode: Some(if uses_proxy {
                    ClaudeDesktopMode::Proxy
                } else {
                    ClaudeDesktopMode::Direct
                }),
                claude_desktop_model_routes: routes,
                is_full_url: (url_mode == UrlMode::FullEndpoint).then_some(true),
                ..ProviderMeta::default()
            });
            return Ok(provider);
        }
        AppType::OpenCode => {
            if url_mode == UrlMode::FullEndpoint {
                return Err(
                    "OpenCode native setup requires a base URL, not a full endpoint".to_string(),
                );
            }
            let npm = match protocol {
                UpstreamProtocol::AnthropicMessages => "@ai-sdk/anthropic",
                UpstreamProtocol::OpenAiChat => "@ai-sdk/openai-compatible",
                UpstreamProtocol::OpenAiResponses => "@ai-sdk/openai",
            };
            let models = normalized_install_models(selection)
                .into_iter()
                .map(|detected| {
                    let name = codex_catalog_display_name(&detected);
                    (
                        detected.id,
                        OpenCodeModel {
                            name,
                            limit: None,
                            options: None,
                            extra: HashMap::new(),
                        },
                    )
                })
                .collect();
            let settings = serde_json::to_value(OpenCodeProviderConfig {
                npm: npm.to_string(),
                name: Some(selection.name.trim().to_string()),
                options: OpenCodeProviderOptions {
                    base_url: Some(base_url),
                    api_key: Some(selection.api_key.clone()),
                    headers: None,
                    extra: HashMap::new(),
                },
                models,
            })
            .map_err(|error| format!("Failed to serialize OpenCode provider: {error}"))?;
            Provider::with_id(
                provider_id.to_string(),
                selection.name.clone(),
                settings,
                None,
            )
        }
        _ => return Err(format!("Unsupported wizard app: {}", app.as_str())),
    };

    let mut meta = ProviderMeta {
        api_format: Some(protocol_string(protocol).to_string()),
        is_full_url: (url_mode == UrlMode::FullEndpoint).then_some(true),
        ..ProviderMeta::default()
    };
    if app == AppType::Claude {
        meta.api_key_field = Some(if protocol == UpstreamProtocol::AnthropicMessages {
            "ANTHROPIC_API_KEY".to_string()
        } else {
            "ANTHROPIC_AUTH_TOKEN".to_string()
        });
    } else if protocol == UpstreamProtocol::AnthropicMessages {
        meta.api_key_field = Some("ANTHROPIC_API_KEY".to_string());
        meta.impersonate_claude_code = Some(false);
    }
    provider.meta = Some(meta);
    Ok(provider)
}

fn build_claude_desktop_model_routes(
    models: &[DetectedModel],
    proxy: bool,
) -> HashMap<String, ClaudeDesktopModelRoute> {
    let mut routes = HashMap::new();
    for (index, detected) in models.iter().enumerate() {
        let route_id =
            if !proxy && crate::claude_desktop_config::is_claude_safe_model_id(&detected.id) {
                detected.id.clone()
            } else {
                let base = match index % 4 {
                    0 => "claude-sonnet-4-6",
                    1 => "claude-opus-4-6",
                    2 => "claude-haiku-4-5",
                    _ => "claude-fable-5",
                };
                if index < 4 {
                    base.to_string()
                } else {
                    format!("{base}-r{}", index / 4 + 1)
                }
            };
        routes.insert(
            route_id,
            ClaudeDesktopModelRoute {
                model: detected.id.clone(),
                label_override: Some(codex_catalog_display_name(detected)),
                supports_1m: None,
            },
        );
    }
    routes
}

fn protocol_string(protocol: UpstreamProtocol) -> &'static str {
    match protocol {
        UpstreamProtocol::AnthropicMessages => "anthropic",
        UpstreamProtocol::OpenAiChat => "openai_chat",
        UpstreamProtocol::OpenAiResponses => "openai_responses",
    }
}

fn build_codex_config_text(
    provider_id: &str,
    provider_name: &str,
    base_url: &str,
    model: &str,
) -> String {
    let mut doc = DocumentMut::new();
    doc["model_provider"] = value(provider_id);
    doc["model"] = value(model);

    let mut provider = Table::new();
    provider["name"] = value(provider_name);
    provider["base_url"] = value(base_url);
    provider["wire_api"] = value("responses");

    let mut providers = Table::new();
    providers.set_implicit(true);
    providers.insert(provider_id, Item::Table(provider));
    doc["model_providers"] = Item::Table(providers);
    doc.to_string()
}

fn normalized_install_models(selection: &ProviderInstallSelection) -> Vec<DetectedModel> {
    let mut models = Vec::new();
    let mut seen = HashSet::new();

    for detected in &selection.models {
        let id = detected.id.trim();
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        models.push(DetectedModel {
            id: id.to_string(),
            owned_by: detected
                .owned_by
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            display_name: detected
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        });
    }

    let default_model = selection.model.trim();
    if !default_model.is_empty() && seen.insert(default_model.to_string()) {
        models.push(DetectedModel {
            id: default_model.to_string(),
            owned_by: None,
            display_name: None,
        });
    }
    models
}

fn codex_catalog_display_name(model: &DetectedModel) -> String {
    let Some(display_name) = model
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != model.id)
    else {
        return model.id.clone();
    };
    if display_name.contains(&model.id) {
        display_name.to_string()
    } else {
        format!("{display_name} ({})", model.id)
    }
}

fn build_codex_model_catalog(selection: &ProviderInstallSelection) -> Value {
    let models = normalized_install_models(selection)
        .into_iter()
        .map(|model| {
            serde_json::json!({
                "model": model.id,
                "displayName": codex_catalog_display_name(&model),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "models": models })
}

fn build_claude_preview(
    selection: &ProviderInstallSelection,
    provider_id: &str,
    protocol: UpstreamProtocol,
    model: &str,
) -> AppInstallPreview {
    let proxy = protocol != UpstreamProtocol::AnthropicMessages;
    let auth_field = if protocol == UpstreamProtocol::AnthropicMessages {
        "ANTHROPIC_API_KEY"
    } else {
        "ANTHROPIC_AUTH_TOKEN"
    };
    let redacted_config = serde_json::json!({
        "env": {
            "ANTHROPIC_BASE_URL": selection.base_url.trim().trim_end_matches('/'),
            auth_field: redact_secret(&selection.api_key),
            "ANTHROPIC_MODEL": model,
        },
        "meta": {
            "apiFormat": protocol,
            "apiKeyField": auth_field,
        }
    });
    let mut warnings = Vec::new();
    if proxy {
        warnings
            .push("Claude Code must use CC Switch local routing for this protocol.".to_string());
    }
    AppInstallPreview {
        app: AppType::Claude,
        provider_id: provider_id.to_string(),
        protocol,
        mode: if proxy { "proxy" } else { "direct" }.to_string(),
        model: model.to_string(),
        files_to_change: vec![config::get_claude_settings_path().display().to_string()],
        restart_required: proxy,
        redacted_config,
        warnings,
    }
}

fn build_codex_preview(
    selection: &ProviderInstallSelection,
    provider_id: &str,
    protocol: UpstreamProtocol,
    model: &str,
) -> AppInstallPreview {
    let proxy = protocol != UpstreamProtocol::OpenAiResponses;
    let base_url = selection.base_url.trim().trim_end_matches('/');
    let config_text = build_codex_config_text(provider_id, selection.name.trim(), base_url, model);
    let redacted_config = serde_json::json!({
        "auth": {"OPENAI_API_KEY": redact_secret(&selection.api_key)},
        "config": config_text,
        "modelCatalog": build_codex_model_catalog(selection),
        "meta": {"apiFormat": protocol}
    });
    let mut warnings = Vec::new();
    if proxy {
        warnings.push("Codex must use CC Switch local routing for this protocol.".to_string());
    }
    AppInstallPreview {
        app: AppType::Codex,
        provider_id: provider_id.to_string(),
        protocol,
        mode: if proxy { "proxy" } else { "direct" }.to_string(),
        model: model.to_string(),
        files_to_change: vec![
            codex_config::get_codex_auth_path().display().to_string(),
            codex_config::get_codex_config_path().display().to_string(),
        ],
        restart_required: true,
        redacted_config,
        warnings,
    }
}

fn build_claude_desktop_preview(
    selection: &ProviderInstallSelection,
    provider_id: &str,
    protocol: UpstreamProtocol,
    model: &str,
) -> AppInstallPreview {
    let models = normalized_install_models(selection);
    let proxy = protocol != UpstreamProtocol::AnthropicMessages
        || models
            .iter()
            .any(|detected| !crate::claude_desktop_config::is_claude_safe_model_id(&detected.id));
    let routes = build_claude_desktop_model_routes(&models, proxy);
    let route_count = routes.len();
    let mut warnings = Vec::new();
    if proxy {
        warnings.push(
            "Claude Cowork will use the CC Switch local gateway so every discovered model can be routed safely."
                .to_string(),
        );
    }
    AppInstallPreview {
        app: AppType::ClaudeDesktop,
        provider_id: provider_id.to_string(),
        protocol,
        mode: if proxy { "proxy" } else { "direct" }.to_string(),
        model: model.to_string(),
        files_to_change: crate::claude_desktop_config::get_config_library_path()
            .map(|path| vec![path.display().to_string()])
            .unwrap_or_default(),
        restart_required: true,
        redacted_config: serde_json::json!({
            "env": {
                "ANTHROPIC_BASE_URL": selection.base_url.trim().trim_end_matches('/'),
                "ANTHROPIC_AUTH_TOKEN": redact_secret(&selection.api_key),
            },
            "meta": {
                "apiFormat": protocol,
                "claudeDesktopMode": if proxy { "proxy" } else { "direct" },
                "modelRouteCount": route_count,
            }
        }),
        warnings,
    }
}

fn build_opencode_preview(
    selection: &ProviderInstallSelection,
    provider_id: &str,
    protocol: UpstreamProtocol,
    model: &str,
) -> Result<AppInstallPreview, String> {
    let (_, url_mode, _) = normalize_base_url(&selection.base_url)?;
    if url_mode == UrlMode::FullEndpoint {
        return Err("OpenCode native setup requires a base URL, not a full endpoint".to_string());
    }
    let npm = match protocol {
        UpstreamProtocol::AnthropicMessages => "@ai-sdk/anthropic",
        UpstreamProtocol::OpenAiChat => "@ai-sdk/openai-compatible",
        UpstreamProtocol::OpenAiResponses => "@ai-sdk/openai",
    };
    let models = normalized_install_models(selection)
        .into_iter()
        .map(|detected| {
            let display_name = codex_catalog_display_name(&detected);
            (detected.id, display_name)
        })
        .collect::<HashMap<_, _>>();
    Ok(AppInstallPreview {
        app: AppType::OpenCode,
        provider_id: provider_id.to_string(),
        protocol,
        mode: "proxy".to_string(),
        model: model.to_string(),
        files_to_change: vec![crate::opencode_config::get_opencode_config_path()
            .display()
            .to_string()],
        restart_required: false,
        redacted_config: serde_json::json!({
            "npm": npm,
            "name": selection.name.trim(),
            "options": {
                "baseURL": selection.base_url.trim().trim_end_matches('/'),
                "apiKey": redact_secret(&selection.api_key),
            },
            "models": models,
        }),
        warnings: vec![
            "OpenCode keeps the native provider entry and adds the CC Switch local gateway for optional failover."
                .to_string(),
        ],
    })
}

fn wizard_provider_id(name: &str, base_url: &str) -> String {
    use sha2::{Digest, Sha256};

    let slug = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .collect::<String>()
        .to_ascii_lowercase();
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    hasher.update([0]);
    hasher.update(base_url.as_bytes());
    let digest = hasher.finalize();
    let suffix = digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        "wizard-{}-{}",
        if slug.is_empty() { "provider" } else { &slug },
        suffix
    )
}

fn redact_secret(secret: &str) -> String {
    let secret = secret.trim();
    if secret.len() <= 8 {
        return "********".to_string();
    }
    format!("{}...{}", &secret[..3], &secret[secret.len() - 4..])
}

fn normalize_base_url(raw: &str) -> Result<(String, UrlMode, Option<EndpointCandidate>), String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Base URL is required".to_string());
    }

    let parsed = Url::parse(trimmed).map_err(|error| format!("Invalid base URL: {error}"))?;
    if !matches!(parsed.scheme(), "https" | "http") {
        return Err("Base URL must use http or https".to_string());
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err("Base URL must not contain credentials".to_string());
    }
    if parsed.fragment().is_some() {
        return Err("Base URL must not contain a fragment".to_string());
    }
    if parsed.scheme() == "http"
        && !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
    {
        return Err("HTTP is only allowed for localhost during provider setup".to_string());
    }

    let detected = ENDPOINTS.iter().copied().find(|candidate| {
        parsed
            .path()
            .trim_end_matches('/')
            .ends_with(candidate.path)
    });
    let mode = if detected.is_some() {
        UrlMode::FullEndpoint
    } else {
        UrlMode::Base
    };
    Ok((trimmed.to_string(), mode, detected))
}

fn endpoint_url(base_url: &str, mode: UrlMode, path: &str) -> Result<String, String> {
    if mode == UrlMode::FullEndpoint {
        return Ok(base_url.to_string());
    }

    let mut base = Url::parse(base_url).map_err(|error| format!("Invalid base URL: {error}"))?;
    let current_path = base.path().trim_end_matches('/');
    let endpoint_suffix = path.strip_prefix("/v1").unwrap_or(path);
    let has_version_suffix = current_path
        .rsplit('/')
        .next()
        .and_then(|segment| segment.strip_prefix('v'))
        .is_some_and(|version| !version.is_empty() && version.chars().all(|c| c.is_ascii_digit()));
    let joined_path = if has_version_suffix {
        format!("{current_path}{endpoint_suffix}")
    } else {
        format!("{current_path}/v1{endpoint_suffix}")
    };
    base.set_path(&joined_path);
    Ok(base.to_string().trim_end_matches('/').to_string())
}

async fn fetch_models_for_probe(
    base_url: &str,
    url_mode: UrlMode,
    models_url_override: Option<&str>,
    api_key: &str,
) -> Result<Vec<DetectedModel>, String> {
    let candidates = build_models_url_candidates(
        base_url,
        url_mode == UrlMode::FullEndpoint,
        models_url_override,
    )?;
    let client = http_client::get();
    let mut last_error = None;

    for url in candidates {
        for auth_mode in [AuthMode::Bearer, AuthMode::XApiKey] {
            let response = match client
                .get(&url)
                .headers(auth_headers(api_key, auth_mode)?)
                .header(CONNECTION, "close")
                .timeout(PROBE_TIMEOUT)
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    last_error = Some(format!("Model discovery request failed: {error}"));
                    continue;
                }
            };
            let status = response.status();
            if !status.is_success() {
                last_error = Some(format!("Model discovery failed with HTTP {status}"));
                continue;
            }
            let body = response
                .bytes()
                .await
                .map_err(|error| format!("Model discovery response failed: {error}"))?;
            if body.len() > MAX_MODELS_BODY_BYTES {
                return Err("Model discovery response is too large".to_string());
            }
            let value: Value = serde_json::from_slice(&body)
                .map_err(|error| format!("Model discovery response is invalid JSON: {error}"))?;
            match parse_models_response(&value) {
                Ok(models) => return Ok(models),
                Err(error) => last_error = Some(error),
            }
        }
    }

    Err(last_error.unwrap_or_else(|| "Model discovery failed".to_string()))
}

fn parse_models_response(value: &Value) -> Result<Vec<DetectedModel>, String> {
    let mut models = Vec::new();
    let mut recognized = false;

    if let Some(entries) = value.as_array() {
        recognized = true;
        for entry in entries {
            push_detected_model(&mut models, entry, None);
        }
    } else {
        for key in ["data", "models", "items"] {
            if let Some(entries) = value.get(key).and_then(Value::as_array) {
                recognized = true;
                for entry in entries {
                    push_detected_model(&mut models, entry, None);
                }
                break;
            }
        }

        if !recognized {
            if let Some(data) = value.get("data") {
                for key in ["models", "items"] {
                    if let Some(entries) = data.get(key).and_then(Value::as_array) {
                        recognized = true;
                        for entry in entries {
                            push_detected_model(&mut models, entry, None);
                        }
                        break;
                    }
                }
            }
        }

        if !recognized {
            if let Some(model_map) = value.get("models").and_then(Value::as_object) {
                recognized = true;
                for (id, entry) in model_map {
                    push_detected_model(&mut models, entry, Some(id));
                }
            }
        }
    }

    if !recognized {
        return Err("Model discovery response does not contain data, models, or items".to_string());
    }

    let mut seen = HashSet::new();
    models.retain(|model| seen.insert(model.id.clone()));
    if models.len() > MAX_DISCOVERED_MODELS {
        return Err(format!(
            "Model discovery returned more than {MAX_DISCOVERED_MODELS} models"
        ));
    }
    Ok(models)
}

fn push_detected_model(models: &mut Vec<DetectedModel>, entry: &Value, fallback_id: Option<&str>) {
    if let Some(id) = entry.as_str().map(str::trim).filter(|id| !id.is_empty()) {
        models.push(DetectedModel {
            id: id.to_string(),
            owned_by: None,
            display_name: None,
        });
        return;
    }

    let Some(object) = entry.as_object() else {
        if let Some(id) = fallback_id.map(str::trim).filter(|id| !id.is_empty()) {
            models.push(DetectedModel {
                id: id.to_string(),
                owned_by: None,
                display_name: None,
            });
        }
        return;
    };
    let Some(id) = model_string_field(object, &["id", "model", "slug", "name"]).or_else(|| {
        fallback_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    }) else {
        return;
    };

    models.push(DetectedModel {
        id,
        owned_by: model_string_field(
            object,
            &["owned_by", "ownedBy", "owner", "provider", "vendor"],
        ),
        display_name: model_string_field(object, &["display_name", "displayName", "name"]),
    });
}

fn model_string_field(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| object.get(*key))
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

async fn probe_endpoint(
    client: &reqwest::Client,
    endpoint: &str,
    candidate: EndpointCandidate,
    api_key: &str,
    model: &str,
) -> ProtocolCapability {
    let auth_modes = [
        candidate.preferred_auth,
        alternate_auth(candidate.preferred_auth),
    ];
    let mut evidence = Vec::new();

    for auth_mode in auth_modes {
        let body = match candidate.protocol {
            UpstreamProtocol::AnthropicMessages | UpstreamProtocol::OpenAiChat => {
                serde_json::json!({
                    "model": model,
                    "max_tokens": 1,
                    "stream": false,
                    "messages": [{"role": "user", "content": "ping"}]
                })
            }
            UpstreamProtocol::OpenAiResponses => serde_json::json!({
                "model": model,
                "max_output_tokens": 1,
                "stream": false,
                "input": "ping"
            }),
        };
        let request = client
            .post(endpoint)
            .headers(match auth_headers(api_key, auth_mode) {
                Ok(headers) => headers,
                Err(error) => {
                    evidence.push(error);
                    continue;
                }
            })
            .header("content-type", "application/json")
            .header(CONNECTION, "close")
            .json(&body)
            .timeout(PROBE_TIMEOUT);

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                evidence.push(format!("{auth_mode:?}: network error: {error}"));
                continue;
            }
        };
        let status = response.status();
        if status.is_success() {
            evidence.push(format!("{auth_mode:?}: HTTP {status}"));
            return ProtocolCapability {
                protocol: candidate.protocol,
                endpoint: endpoint.to_string(),
                auth_mode,
                supported: true,
                confidence: ProbeConfidence::High,
                evidence,
            };
        }
        if matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ) || status == StatusCode::TOO_MANY_REQUESTS
        {
            evidence.push(format!(
                "{auth_mode:?}: HTTP {status}, endpoint accepted request"
            ));
            return ProtocolCapability {
                protocol: candidate.protocol,
                endpoint: endpoint.to_string(),
                auth_mode,
                supported: true,
                confidence: ProbeConfidence::Medium,
                evidence,
            };
        }
        evidence.push(format!("{auth_mode:?}: HTTP {status}"));
    }

    ProtocolCapability {
        protocol: candidate.protocol,
        endpoint: endpoint.to_string(),
        auth_mode: candidate.preferred_auth,
        supported: false,
        confidence: ProbeConfidence::Low,
        evidence,
    }
}

fn auth_headers(api_key: &str, auth_mode: AuthMode) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    let value = HeaderValue::from_str(api_key)
        .map_err(|error| format!("Invalid API key header value: {error}"))?;
    match auth_mode {
        AuthMode::Bearer => {
            let bearer = HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|error| format!("Invalid bearer header value: {error}"))?;
            headers.insert(AUTHORIZATION, bearer);
        }
        AuthMode::XApiKey => {
            headers.insert(HeaderName::from_static("x-api-key"), value);
            headers.insert(
                HeaderName::from_static("anthropic-version"),
                HeaderValue::from_static("2023-06-01"),
            );
        }
    }
    Ok(headers)
}

fn alternate_auth(auth_mode: AuthMode) -> AuthMode {
    match auth_mode {
        AuthMode::Bearer => AuthMode::XApiKey,
        AuthMode::XApiKey => AuthMode::Bearer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use serde_json::json;
    use serial_test::serial;
    use std::env;
    use tempfile::TempDir;
    use tokio::sync::oneshot;

    struct TempHome {
        _dir: TempDir,
        home: Option<String>,
        userprofile: Option<String>,
        test_home: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().expect("create temp home");
            let home = env::var("HOME").ok();
            let userprofile = env::var("USERPROFILE").ok();
            let test_home = env::var("CC_SWITCH_TEST_HOME").ok();
            env::set_var("HOME", dir.path());
            env::set_var("USERPROFILE", dir.path());
            env::set_var("CC_SWITCH_TEST_HOME", dir.path());
            crate::settings::reload_settings().expect("reload settings");
            Self {
                _dir: dir,
                home,
                userprofile,
                test_home,
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
            match &self.userprofile {
                Some(value) => env::set_var("USERPROFILE", value),
                None => env::remove_var("USERPROFILE"),
            }
            match &self.test_home {
                Some(value) => env::set_var("CC_SWITCH_TEST_HOME", value),
                None => env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    #[test]
    fn normalizes_supported_full_endpoints() {
        let (_, mode, protocol) = normalize_base_url("https://api.example/v1/responses/").unwrap();
        assert_eq!(mode, UrlMode::FullEndpoint);
        assert_eq!(
            protocol.unwrap().protocol,
            UpstreamProtocol::OpenAiResponses
        );
    }

    #[test]
    fn rejects_remote_http_and_embedded_credentials() {
        assert!(normalize_base_url("http://api.example/v1").is_err());
        assert!(normalize_base_url("https://user:pass@api.example/v1").is_err());
        assert!(normalize_base_url("https://api.example/v1#fragment").is_err());
    }

    #[test]
    fn appends_v1_without_duplicating_it() {
        assert_eq!(
            endpoint_url("https://api.example", UrlMode::Base, "/v1/responses").unwrap(),
            "https://api.example/v1/responses"
        );
        assert_eq!(
            endpoint_url("https://api.example/v1", UrlMode::Base, "/v1/responses").unwrap(),
            "https://api.example/v1/responses"
        );
        assert_eq!(
            endpoint_url(
                "https://api.example/v4",
                UrlMode::Base,
                "/v1/chat/completions"
            )
            .unwrap(),
            "https://api.example/v4/chat/completions"
        );
        assert_eq!(
            endpoint_url("https://api.example/gateway", UrlMode::Base, "/v1/messages").unwrap(),
            "https://api.example/gateway/v1/messages"
        );
    }

    #[test]
    fn parses_common_model_shapes_with_display_names() {
        let models = parse_models_response(&json!({
            "models": [
                {"slug": "claude-opus-5-thinking", "display_name": "Claude Opus 5", "vendor": "Anthropic"},
                "gpt-5.6-sol"
            ]
        }))
        .expect("parse models shape");

        assert_eq!(
            models,
            vec![
                DetectedModel {
                    id: "claude-opus-5-thinking".to_string(),
                    owned_by: Some("Anthropic".to_string()),
                    display_name: Some("Claude Opus 5".to_string()),
                },
                DetectedModel {
                    id: "gpt-5.6-sol".to_string(),
                    owned_by: None,
                    display_name: None,
                }
            ]
        );
    }

    #[tokio::test]
    async fn probes_models_and_all_supported_protocols_without_exposing_key() {
        async fn models() -> Json<Value> {
            Json(json!({
                "data": [{"id": "test-model", "owned_by": "test"}]
            }))
        }

        async fn inference(Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            if body.get("model").and_then(Value::as_str) == Some("test-model") {
                (StatusCode::OK, Json(json!({"id": "probe-response"})))
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": {"code": "model_not_found"}})),
                )
            }
        }

        let app = Router::new()
            .route("/v1/models", get(models))
            .route("/v1/messages", post(inference))
            .route("/v1/chat/completions", post(inference))
            .route("/v1/responses", post(inference));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind probe server");
        let address = listener.local_addr().expect("probe server address");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("serve probe server");
        });

        let result = probe_provider_capabilities(ProviderProbeInput {
            base_url: format!("http://{address}"),
            api_key: "secret-probe-key".to_string(),
            models_url: None,
            model: None,
            allow_inference_probe: true,
        })
        .await
        .expect("probe provider");

        assert_eq!(result.recommended_model.as_deref(), Some("test-model"));
        assert_eq!(result.capabilities.len(), 3);
        assert!(result
            .capabilities
            .iter()
            .all(|capability| capability.supported));
        assert_eq!(
            result
                .capabilities
                .iter()
                .find(|capability| capability.protocol == UpstreamProtocol::AnthropicMessages)
                .expect("anthropic capability")
                .auth_mode,
            AuthMode::XApiKey
        );
        assert!(result
            .capabilities
            .iter()
            .filter(|capability| capability.protocol != UpstreamProtocol::AnthropicMessages)
            .all(|capability| capability.auth_mode == AuthMode::Bearer));
        assert!(!serde_json::to_string(&result)
            .expect("serialize result")
            .contains("secret-probe-key"));

        let repeated = probe_provider_capabilities(ProviderProbeInput {
            base_url: format!("http://{address}"),
            api_key: "secret-probe-key".to_string(),
            models_url: None,
            model: Some("stale-model-from-previous-provider".to_string()),
            allow_inference_probe: true,
        })
        .await
        .expect("repeat probe with stale model");
        assert_eq!(repeated.recommended_model.as_deref(), Some("test-model"));
        assert!(repeated
            .capabilities
            .iter()
            .all(|capability| capability.supported));
        assert!(repeated
            .warnings
            .iter()
            .any(|warning| warning.contains("not in this provider catalog")));

        let _ = shutdown_tx.send(());
        server.await.expect("join probe server");
    }

    #[test]
    fn preview_generates_redacted_configs_and_routing_requirements() {
        let preview = preview_provider_install(ProviderInstallSelection {
            name: "Example Gateway".to_string(),
            base_url: "https://gateway.example/v1".to_string(),
            api_key: "sk-secret-value-1234".to_string(),
            model: "provider-model".to_string(),
            models: vec![
                DetectedModel {
                    id: "provider-model".to_string(),
                    owned_by: Some("Example".to_string()),
                    display_name: Some("Provider Model".to_string()),
                },
                DetectedModel {
                    id: "provider-model-fast".to_string(),
                    owned_by: Some("Example".to_string()),
                    display_name: Some("Provider Model Fast".to_string()),
                },
            ],
            claude_protocol: Some(UpstreamProtocol::OpenAiChat),
            codex_protocol: Some(UpstreamProtocol::OpenAiResponses),
            claude_desktop_protocol: None,
            opencode_protocol: None,
        })
        .expect("build preview");

        let serialized = serde_json::to_string(&preview).expect("serialize preview");
        assert!(!serialized.contains("sk-secret-value-1234"));
        assert!(preview.proxy_will_start);
        assert_eq!(preview.claude.as_ref().unwrap().mode, "proxy");
        assert_eq!(preview.codex.as_ref().unwrap().mode, "direct");
        assert!(!preview
            .codex
            .as_ref()
            .unwrap()
            .redacted_config
            .to_string()
            .contains("requires_openai_auth"));
        assert_eq!(
            preview.codex.as_ref().unwrap().redacted_config["modelCatalog"]["models"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        assert!(preview
            .claude
            .as_ref()
            .unwrap()
            .redacted_config
            .to_string()
            .contains("sk-...1234"));
    }

    #[test]
    fn cowork_proxy_catalog_routes_every_discovered_model_to_its_upstream_id() {
        let selection = ProviderInstallSelection {
            name: "Gateway".to_string(),
            base_url: "https://gateway.example/v1".to_string(),
            api_key: "secret".to_string(),
            model: "third-party-model".to_string(),
            models: vec![
                DetectedModel {
                    id: "third-party-model".to_string(),
                    owned_by: None,
                    display_name: Some("Third Party Model".to_string()),
                },
                DetectedModel {
                    id: "claude-opus-5".to_string(),
                    owned_by: None,
                    display_name: Some("Claude Opus 5".to_string()),
                },
            ],
            claude_protocol: None,
            codex_protocol: None,
            claude_desktop_protocol: Some(UpstreamProtocol::OpenAiChat),
            opencode_protocol: None,
        };
        let provider = build_provider_for_app(
            &selection,
            "wizard-gateway",
            AppType::ClaudeDesktop,
            UpstreamProtocol::OpenAiChat,
        )
        .expect("build Cowork provider");
        let meta = provider.meta.expect("Cowork metadata");
        assert_eq!(meta.claude_desktop_mode, Some(ClaudeDesktopMode::Proxy));
        assert_eq!(meta.claude_desktop_model_routes.len(), 2);
        assert!(meta
            .claude_desktop_model_routes
            .values()
            .any(|route| route.model == "third-party-model"));
        assert!(meta
            .claude_desktop_model_routes
            .values()
            .any(|route| route.model == "claude-opus-5"));
    }

    #[test]
    fn opencode_provider_keeps_the_full_discovered_model_map() {
        let selection = ProviderInstallSelection {
            name: "Gateway".to_string(),
            base_url: "https://gateway.example/v1".to_string(),
            api_key: "secret".to_string(),
            model: "model-fast".to_string(),
            models: vec![
                DetectedModel {
                    id: "model-fast".to_string(),
                    owned_by: None,
                    display_name: Some("Model Fast".to_string()),
                },
                DetectedModel {
                    id: "model-reasoning".to_string(),
                    owned_by: None,
                    display_name: Some("Model Reasoning".to_string()),
                },
            ],
            claude_protocol: None,
            codex_protocol: None,
            claude_desktop_protocol: None,
            opencode_protocol: Some(UpstreamProtocol::OpenAiChat),
        };
        let provider = build_provider_for_app(
            &selection,
            "wizard-gateway",
            AppType::OpenCode,
            UpstreamProtocol::OpenAiChat,
        )
        .expect("build OpenCode provider");
        assert_eq!(provider.settings_config["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(
            provider.settings_config["models"]
                .as_object()
                .map(|map| map.len()),
            Some(2)
        );
        assert_eq!(
            provider.settings_config["models"]["model-fast"]["name"],
            "Model Fast (model-fast)"
        );
    }

    #[tokio::test]
    #[serial]
    async fn apply_installs_claude_and_codex_for_native_protocols() {
        let _home = TempHome::new();
        let db = std::sync::Arc::new(crate::database::Database::memory().expect("create db"));
        let state = crate::store::AppState::new(db.clone());
        let selection = ProviderInstallSelection {
            name: "Example Gateway".to_string(),
            base_url: "https://gateway.example/v1".to_string(),
            api_key: "sk-secret-value-1234".to_string(),
            model: "provider-model".to_string(),
            models: vec![
                DetectedModel {
                    id: "provider-model".to_string(),
                    owned_by: Some("Example".to_string()),
                    display_name: Some("Provider Model".to_string()),
                },
                DetectedModel {
                    id: "provider-model-fast".to_string(),
                    owned_by: Some("Example".to_string()),
                    display_name: Some("Provider Model Fast".to_string()),
                },
            ],
            claude_protocol: Some(UpstreamProtocol::AnthropicMessages),
            codex_protocol: Some(UpstreamProtocol::OpenAiResponses),
            claude_desktop_protocol: None,
            opencode_protocol: None,
        };

        let result = apply_provider_install(&state, selection)
            .await
            .expect("apply setup");
        assert_eq!(result.applied_apps, vec![AppType::Claude, AppType::Codex]);
        assert!(!result.rolled_back);

        let provider_id = wizard_provider_id("Example Gateway", "https://gateway.example/v1");
        assert!(db
            .get_provider_by_id(&provider_id, "claude")
            .expect("read Claude provider")
            .is_some());
        let codex_provider = db
            .get_provider_by_id(&provider_id, "codex")
            .expect("read Codex provider")
            .expect("Codex provider exists");
        let catalog_models = codex_provider.settings_config["modelCatalog"]["models"]
            .as_array()
            .expect("wizard Codex model catalog");
        assert_eq!(catalog_models.len(), 2);
        assert_eq!(catalog_models[0]["model"], "provider-model");
        assert_eq!(
            catalog_models[0]["displayName"],
            "Provider Model (provider-model)"
        );
        assert_eq!(catalog_models[1]["model"], "provider-model-fast");
        let live_config = crate::codex_config::read_and_validate_codex_config_text()
            .expect("read generated Codex config");
        assert!(live_config.contains("model_catalog_json"));
        let live_catalog: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_model_catalog_path())
                .expect("read generated Codex model catalog");
        assert_eq!(live_catalog["models"].as_array().map(Vec::len), Some(2));
        assert_eq!(live_catalog["models"][0]["slug"], "provider-model");
        assert_eq!(live_catalog["models"][1]["slug"], "provider-model-fast");
        assert_eq!(
            crate::settings::get_effective_current_provider(&db, &AppType::Claude)
                .expect("Claude current"),
            Some(provider_id.clone())
        );
        assert_eq!(
            crate::settings::get_effective_current_provider(&db, &AppType::Codex)
                .expect("Codex current"),
            Some(provider_id)
        );
    }
}
