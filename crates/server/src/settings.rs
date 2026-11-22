//! Provider configuration and model roles, stored in SQLite. API keys are write-only: they are
//! stored for calls but never returned by the API or written to logs.
//!
//! In server mode a provider is either shared (no owner: admins manage it, everyone uses it) or
//! personal (one owner, who alone sees and uses it). Single-user mode and the access token act
//! as [`Principal::Local`] and see every provider.

use openprism_ai::llm::{Model, ReasoningLevel};
use openprism_ai::provider::{ProviderConfig, ProviderKind};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use ts_rs::TS;
use uuid::Uuid;

use axum::http::StatusCode;

use crate::auth::Principal;
use crate::db::now_ms;
use crate::error::ApiError;
use crate::state::AppState;

/// A provider as shown in the UI (without its key).
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: Option<String>,
    pub has_api_key: bool,
    /// Shared by everyone (managed by admins), or personal (server mode). Always true in
    /// single-user mode.
    pub shared: bool,
    pub created_at: i64,
}

/// Static facts about a provider kind, for the settings form.
#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProviderKindInfo {
    pub kind: ProviderKind,
    pub label: String,
    pub needs_api_key: bool,
    pub needs_base_url: bool,
    pub default_base_url: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateProviderRequest {
    pub name: String,
    pub kind: ProviderKind,
    #[ts(optional)]
    pub base_url: Option<String>,
    #[ts(optional)]
    pub api_key: Option<String>,
    /// Server mode: a shared provider (admins only) rather than a personal one. Ignored in
    /// single-user mode, where every provider is shared.
    #[serde(default)]
    #[ts(optional)]
    pub shared: Option<bool>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UpdateProviderRequest {
    #[ts(optional)]
    pub name: Option<String>,
    /// Empty string removes a custom base URL.
    #[ts(optional)]
    pub base_url: Option<String>,
    /// New key; an empty string removes the stored key.
    #[ts(optional)]
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProviderTestResult {
    pub ok: bool,
    pub models: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ModelRef {
    pub provider_id: String,
    pub model: String,
    /// Reasoning effort; absent keeps the provider's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reasoning: Option<ReasoningLevel>,
}

#[derive(Debug, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Roles {
    /// Chat and edits.
    pub agent: Option<ModelRef>,
    /// Quick actions on a selection; falls back to `agent`.
    pub fast: Option<ModelRef>,
}

pub fn kind_label(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Openai => "OpenAI",
        ProviderKind::Anthropic => "Anthropic",
        ProviderKind::Gemini => "Google Gemini",
        ProviderKind::Openrouter => "OpenRouter",
        ProviderKind::Deepseek => "DeepSeek",
        ProviderKind::Mistral => "Mistral",
        ProviderKind::Groq => "Groq",
        ProviderKind::Xai => "xAI",
        ProviderKind::Ollama => "Ollama",
        ProviderKind::OpencodeGo => "OpenCode Go",
        ProviderKind::OpenaiCompatible => "OpenAI-compatible (vLLM, LM Studio, …)",
        ProviderKind::Mock => "Offline mock (testing)",
    }
}

pub fn kinds() -> Vec<ProviderKindInfo> {
    ProviderKind::ALL
        .into_iter()
        .map(|kind| ProviderKindInfo {
            kind,
            label: kind_label(kind).to_owned(),
            needs_api_key: kind.needs_api_key(),
            needs_base_url: kind.needs_base_url(),
            default_base_url: kind.default_base_url().map(str::to_owned),
        })
        .collect()
}

fn view(row: &SqliteRow) -> Result<ProviderView, ApiError> {
    let kind: String = row.try_get("kind")?;
    let key: Option<String> = row.try_get("api_key")?;
    let owner: Option<String> = row.try_get("owner_user_id")?;
    Ok(ProviderView {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        kind: ProviderKind::parse(&kind)
            .ok_or_else(|| ApiError::internal(format!("unknown provider kind {kind}")))?,
        base_url: row.try_get("base_url")?,
        has_api_key: key.is_some_and(|k| !k.is_empty()),
        shared: owner.is_none(),
        created_at: row.try_get("created_at")?,
    })
}

fn clean(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// The providers `principal` may see and use: the shared ones and their own.
pub async fn list(state: &AppState, principal: &Principal) -> Result<Vec<ProviderView>, ApiError> {
    let rows = sqlx::query(
        "SELECT * FROM providers
         WHERE ?1 IS NULL OR owner_user_id IS NULL OR owner_user_id = ?1
         ORDER BY created_at",
    )
    .bind(principal.user_id())
    .fetch_all(&state.db)
    .await?;
    rows.iter().map(view).collect()
}

/// A provider `principal` may see and use; someone else's personal provider does not exist for
/// them (404).
async fn visible_row(
    state: &AppState,
    principal: &Principal,
    id: &str,
) -> Result<SqliteRow, ApiError> {
    sqlx::query(
        "SELECT * FROM providers
         WHERE id = ?1 AND (?2 IS NULL OR owner_user_id IS NULL OR owner_user_id = ?2)",
    )
    .bind(id)
    .bind(principal.user_id())
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("no such provider"))
}

/// A provider `principal` may change: their own, or a shared one for admins.
async fn managed_row(
    state: &AppState,
    principal: &Principal,
    id: &str,
) -> Result<SqliteRow, ApiError> {
    let row = visible_row(state, principal, id).await?;
    let owner: Option<String> = row.try_get("owner_user_id")?;
    if owner.is_none() && !principal.is_admin() {
        return Err(shared_needs_admin());
    }
    Ok(row)
}

fn shared_needs_admin() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "admins_only",
        "only an admin can add or change a shared provider; add your own key as a personal provider",
    )
}

pub async fn get_for(
    state: &AppState,
    principal: &Principal,
    id: &str,
) -> Result<ProviderView, ApiError> {
    view(&visible_row(state, principal, id).await?)
}

pub async fn get(state: &AppState, id: &str) -> Result<ProviderView, ApiError> {
    let row = sqlx::query("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("no such provider"))?;
    view(&row)
}

/// The full configuration, key included, for making calls.
pub async fn config(state: &AppState, id: &str) -> Result<ProviderConfig, ApiError> {
    let row = sqlx::query("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("no such provider"))?;
    let kind: String = row.try_get("kind")?;
    Ok(ProviderConfig {
        id: row.try_get("id")?,
        kind: ProviderKind::parse(&kind)
            .ok_or_else(|| ApiError::internal(format!("unknown provider kind {kind}")))?,
        base_url: row.try_get("base_url")?,
        api_key: row.try_get("api_key")?,
    })
}

fn check(config: &ProviderConfig) -> Result<(), ApiError> {
    config
        .validate()
        .map_err(|err| ApiError::bad_request(err.to_string()))
}

/// A new provider: personal for an account unless it asks for a shared one (admins only);
/// shared in single-user mode and for the access token.
pub async fn create(
    state: &AppState,
    principal: &Principal,
    request: CreateProviderRequest,
) -> Result<ProviderView, ApiError> {
    let owner = match principal.user() {
        None => None,
        Some(_) if request.shared == Some(true) => {
            if !principal.is_admin() {
                return Err(shared_needs_admin());
            }
            None
        }
        Some(user) => Some(user.id.clone()),
    };
    let name = clean(Some(request.name)).unwrap_or_else(|| kind_label(request.kind).to_owned());
    let config = ProviderConfig {
        id: Uuid::now_v7().to_string(),
        kind: request.kind,
        base_url: clean(request.base_url),
        api_key: clean(request.api_key),
    };
    check(&config)?;
    let now = now_ms();
    sqlx::query(
        "INSERT INTO providers (id, name, kind, base_url, api_key, owner_user_id, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&config.id)
    .bind(&name)
    .bind(config.kind.as_str())
    .bind(&config.base_url)
    .bind(&config.api_key)
    .bind(&owner)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await?;
    get(state, &config.id).await
}

pub async fn update(
    state: &AppState,
    principal: &Principal,
    id: &str,
    request: UpdateProviderRequest,
) -> Result<ProviderView, ApiError> {
    managed_row(state, principal, id).await?;
    let mut config = config(state, id).await?;
    let current = get(state, id).await?;
    let name = clean(request.name).unwrap_or(current.name);
    if let Some(url) = request.base_url {
        config.base_url = clean(Some(url));
    }
    if let Some(key) = request.api_key {
        config.api_key = clean(Some(key));
    }
    check(&config)?;
    sqlx::query(
        "UPDATE providers SET name = ?, base_url = ?, api_key = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&name)
    .bind(&config.base_url)
    .bind(&config.api_key)
    .bind(now_ms())
    .bind(id)
    .execute(&state.db)
    .await?;
    // A different endpoint may serve different models.
    sqlx::query("DELETE FROM models WHERE provider_id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    get(state, id).await
}

pub async fn delete(state: &AppState, principal: &Principal, id: &str) -> Result<(), ApiError> {
    managed_row(state, principal, id).await?;
    let deleted = sqlx::query("DELETE FROM providers WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::not_found("no such provider"));
    }
    Ok(())
}

pub async fn test(
    state: &AppState,
    principal: &Principal,
    id: &str,
) -> Result<ProviderTestResult, ApiError> {
    visible_row(state, principal, id).await?;
    let config = config(state, id).await?;
    Ok(match openprism_ai::provider::list_models(&config).await {
        Ok(models) => ProviderTestResult {
            ok: true,
            models,
            error: None,
        },
        Err(err) => ProviderTestResult {
            ok: false,
            models: Vec::new(),
            error: Some(err.to_string()),
        },
    })
}

pub async fn roles(state: &AppState) -> Result<Roles, ApiError> {
    let rows = sqlx::query("SELECT role, provider_id, model, reasoning FROM roles")
        .fetch_all(&state.db)
        .await?;
    let mut roles = Roles::default();
    for row in rows {
        let role: String = row.try_get("role")?;
        let reasoning: Option<String> = row.try_get("reasoning")?;
        let model = ModelRef {
            provider_id: row.try_get("provider_id")?,
            model: row.try_get("model")?,
            reasoning: reasoning.as_deref().and_then(ReasoningLevel::parse),
        };
        match role.as_str() {
            "agent" => roles.agent = Some(model),
            "fast" => roles.fast = Some(model),
            _ => {}
        }
    }
    Ok(roles)
}

pub async fn set_roles(state: &AppState, roles: Roles) -> Result<Roles, ApiError> {
    for (role, model) in [("agent", roles.agent), ("fast", roles.fast)] {
        match model {
            Some(model) => {
                get(state, &model.provider_id).await?;
                if model.model.trim().is_empty() {
                    return Err(ApiError::bad_request(format!(
                        "choose a model for the {role} role"
                    )));
                }
                sqlx::query(
                    "INSERT INTO roles (role, provider_id, model, reasoning) VALUES (?, ?, ?, ?)
                     ON CONFLICT (role) DO UPDATE SET provider_id = excluded.provider_id,
                         model = excluded.model, reasoning = excluded.reasoning",
                )
                .bind(role)
                .bind(&model.provider_id)
                .bind(model.model.trim())
                .bind(model.reasoning.map(ReasoningLevel::as_str))
                .execute(&state.db)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM roles WHERE role = ?")
                    .bind(role)
                    .execute(&state.db)
                    .await?;
            }
        }
    }
    self::roles(state).await
}

/// Whether the model is known to reject tool calls.
pub async fn supports_tools(state: &AppState, model: &ModelRef) -> Result<Option<bool>, ApiError> {
    let value: Option<Option<i64>> =
        sqlx::query_scalar("SELECT supports_tools FROM models WHERE provider_id = ? AND model = ?")
            .bind(&model.provider_id)
            .bind(&model.model)
            .fetch_optional(&state.db)
            .await?;
    Ok(value.flatten().map(|v| v != 0))
}

pub async fn remember_tools(
    state: &AppState,
    model: &ModelRef,
    supported: bool,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO models (provider_id, model, supports_tools) VALUES (?, ?, ?)
         ON CONFLICT (provider_id, model) DO UPDATE SET supports_tools = excluded.supports_tools",
    )
    .bind(&model.provider_id)
    .bind(&model.model)
    .bind(i64::from(supported))
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Builds a model client for `model`.
pub async fn model(state: &AppState, model: &ModelRef) -> Result<Model, ApiError> {
    let config = config(state, &model.provider_id).await?;
    Model::new(&config, &model.model).map_err(|err| ApiError::bad_request(err.to_string()))
}
