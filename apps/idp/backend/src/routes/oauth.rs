//! OIDC discovery + OAuth 2.0 authorization-code (with mandatory PKCE)
//! endpoints. Every client is a *public* client (no `client_secret` --
//! see docs/architecture.md), and there is no consent screen: every client
//! is trusted, whether declared in the IDP's static registry (`clients.rs`,
//! `IDP_CLIENTS_JSON`) or created ad hoc through `/admin` (`db::create_client`),
//! so a valid session goes straight from `/oauth/authorize` back to the
//! relying party with a code, no extra approval click. A client with
//! `access_restricted = true` still gates *which* users get that code at
//! all -- see `issue_code_redirect`.

use axum::{
    extract::{Form, Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use axum_extra::extract::cookie::PrivateCookieJar;
use serde::{Deserialize, Serialize};

use crate::{
    clients, db,
    error::AppError,
    keys::{verify_pkce_s256, Claims},
    metrics,
    state::AppState,
};

fn extract_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-real-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

// ── OIDC discovery ────────────────────────────────────────────────────────────

pub async fn openid_configuration(State(app): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "issuer": app.base_url,
        "authorization_endpoint": format!("{}/oauth/authorize", app.base_url),
        "token_endpoint": format!("{}/oauth/token", app.base_url),
        "userinfo_endpoint": format!("{}/oauth/userinfo", app.base_url),
        "revocation_endpoint": format!("{}/oauth/revoke", app.base_url),
        "jwks_uri": format!("{}/.well-known/jwks.json", app.base_url),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "scopes_supported": ["openid", "profile"],
        "token_endpoint_auth_methods_supported": ["none"],
        "claims_supported": ["sub", "iss", "aud", "exp", "iat", "auth_time", "nonce", "scope", "name", "preferred_username", "roles"],
        "code_challenge_methods_supported": ["S256", "plain"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "prompt_values_supported": ["none", "login"],
    }))
}

pub async fn jwks(State(app): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "keys": [{
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": app.jwt_keys.kid,
            "n": app.jwt_keys.n_b64url,
            "e": app.jwt_keys.e_b64url,
        }]
    }))
}

// ── Authorization endpoint ────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct AuthorizeQuery {
    pub response_type: Option<String>,
    pub client_id: Option<String>,
    pub redirect_uri: Option<String>,
    pub scope: Option<String>,
    pub state: Option<String>,
    pub nonce: Option<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub prompt: Option<String>,
    pub max_age: Option<i64>,
}

/// `GET /oauth/authorize`
pub async fn authorize(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    Query(params): Query<AuthorizeQuery>,
) -> Result<Response, AppError> {
    authorize_inner(&app, jar, params).await
}

/// `POST /oauth/authorize` -- same logic, parameters from the form body
/// (used by the login page's redirect-back after a successful ceremony).
pub async fn authorize_post(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    Form(params): Form<AuthorizeQuery>,
) -> Result<Response, AppError> {
    authorize_inner(&app, jar, params).await
}

async fn authorize_inner(
    app: &AppState,
    jar: PrivateCookieJar,
    params: AuthorizeQuery,
) -> Result<Response, AppError> {
    let client_id = params
        .client_id
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("client_id required".into()))?;
    let redirect_uri = params
        .redirect_uri
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("redirect_uri required".into()))?;

    let client = db::get_client(&app.db, client_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("unknown client_id".into()))?;
    if !clients::redirect_uri_allowed(&client, redirect_uri) {
        return Err(AppError::BadRequest(
            "redirect_uri not allowed for this client".into(),
        ));
    }

    let response_type = params.response_type.as_deref().unwrap_or("");
    if response_type != "code" {
        let err = if response_type.is_empty() {
            "invalid_request"
        } else {
            "unsupported_response_type"
        };
        return Ok(auth_error_redirect(
            redirect_uri,
            err,
            params.state.as_deref(),
        ));
    }

    let scope = params.scope.as_deref().unwrap_or("openid");
    let prompt = params.prompt.as_deref().unwrap_or("");

    let session_id = jar.get("session").map(|c| c.value().to_string());
    let current_session = match session_id {
        Some(sid) => db::get_session(&app.db, &sid).await?,
        None => None,
    };

    if prompt == "none" {
        return match &current_session {
            None => Ok(auth_error_redirect(
                redirect_uri,
                "login_required",
                params.state.as_deref(),
            )),
            Some(s) => {
                issue_code_redirect(
                    app,
                    &client,
                    &s.user_id,
                    redirect_uri,
                    &params,
                    scope,
                    s.created_at.timestamp(),
                )
                .await
            }
        };
    }

    let force_reauth = match &current_session {
        Some(s) => {
            let age = chrono::Utc::now()
                .signed_duration_since(s.created_at)
                .num_seconds();
            let over_max_age = params.max_age.is_some_and(|ma| age > ma);
            prompt == "login" || over_max_age
        }
        None => false,
    };

    if force_reauth {
        if let Some(s) = &current_session {
            let _ = db::delete_session_by_id(&app.db, &s.id).await;
        }
        let next = build_authorize_url(&app.base_url, client_id, redirect_uri, &params, false);
        return Ok(Redirect::to(&format!("/login?next={}", url_encode(&next))).into_response());
    }

    let Some(session) = current_session else {
        let next = build_authorize_url(&app.base_url, client_id, redirect_uri, &params, true);
        return Ok(Redirect::to(&format!("/login?next={}", url_encode(&next))).into_response());
    };

    issue_code_redirect(
        app,
        &client,
        &session.user_id,
        redirect_uri,
        &params,
        scope,
        session.created_at.timestamp(),
    )
    .await
}

fn auth_error_redirect(redirect_uri: &str, error: &str, state: Option<&str>) -> Response {
    let mut url = format!("{}?error={}", redirect_uri, url_encode(error));
    if let Some(s) = state {
        url.push_str(&format!("&state={}", url_encode(s)));
    }
    Redirect::to(&url).into_response()
}

async fn issue_code_redirect(
    app: &AppState,
    client: &db::Client,
    user_id: &str,
    redirect_uri: &str,
    params: &AuthorizeQuery,
    scope: &str,
    auth_time: i64,
) -> Result<Response, AppError> {
    if client.access_restricted && !db::user_has_access(&app.db, user_id, &client.client_id).await?
    {
        return Ok(auth_error_redirect(
            redirect_uri,
            "access_denied",
            params.state.as_deref(),
        ));
    }

    let code = db::create_authorization_code(
        &app.db,
        &client.client_id,
        user_id,
        redirect_uri,
        scope,
        params.nonce.as_deref(),
        params.code_challenge.as_deref(),
        params.code_challenge_method.as_deref(),
        auth_time,
    )
    .await?;

    let mut url = format!("{redirect_uri}?code={code}");
    if let Some(s) = &params.state {
        url.push_str(&format!("&state={}", url_encode(s)));
    }
    Ok(Redirect::to(&url).into_response())
}

/// Rebuilds the authorize URL to send the browser back to after `/login`
/// completes. `keep_prompt=false` drops `prompt` so a forced-reauth
/// redirect can't loop.
fn build_authorize_url(
    base_url: &str,
    client_id: &str,
    redirect_uri: &str,
    p: &AuthorizeQuery,
    keep_prompt: bool,
) -> String {
    let mut url = format!(
        "{base_url}/oauth/authorize?response_type=code&client_id={}&redirect_uri={}",
        url_encode(client_id),
        url_encode(redirect_uri),
    );
    if let Some(s) = &p.scope {
        url.push_str(&format!("&scope={}", url_encode(s)));
    }
    if let Some(s) = &p.state {
        url.push_str(&format!("&state={}", url_encode(s)));
    }
    if let Some(s) = &p.nonce {
        url.push_str(&format!("&nonce={}", url_encode(s)));
    }
    if let Some(s) = &p.code_challenge {
        url.push_str(&format!("&code_challenge={}", url_encode(s)));
    }
    if let Some(s) = &p.code_challenge_method {
        url.push_str(&format!("&code_challenge_method={}", url_encode(s)));
    }
    if keep_prompt {
        if let Some(s) = &p.prompt {
            url.push_str(&format!("&prompt={}", url_encode(s)));
        }
    }
    url
}

fn url_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

// ── Token endpoint ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub code: Option<String>,
    pub redirect_uri: Option<String>,
    pub code_verifier: Option<String>,
    pub client_id: Option<String>,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub scope: String,
}

pub async fn token(
    State(app): State<AppState>,
    headers: HeaderMap,
    Form(req): Form<TokenRequest>,
) -> Result<impl IntoResponse, AppError> {
    let ip = extract_ip(&headers);
    if !app.token_limiter.check(&ip) {
        metrics::rate_limited("token");
        return Err(AppError::RateLimited);
    }

    let result = match req.grant_type.as_str() {
        "authorization_code" => token_authorization_code(&app, req).await?,
        "refresh_token" => token_refresh(&app, req).await?,
        _ => return Err(AppError::BadRequest("unsupported grant_type".into())),
    };

    // RFC 6749 §5.1 -- token responses MUST include Cache-Control: no-store.
    let mut response = result.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        HeaderName::from_static("pragma"),
        HeaderValue::from_static("no-cache"),
    );
    Ok(response)
}

async fn token_authorization_code(
    app: &AppState,
    req: TokenRequest,
) -> Result<Json<TokenResponse>, AppError> {
    let code = req
        .code
        .ok_or_else(|| AppError::BadRequest("code required".into()))?;
    let redirect_uri = req
        .redirect_uri
        .ok_or_else(|| AppError::BadRequest("redirect_uri required".into()))?;
    let client_id = req
        .client_id
        .ok_or_else(|| AppError::BadRequest("client_id required".into()))?;

    db::get_client(&app.db, &client_id)
        .await?
        .ok_or_else(|| AppError::Unauthorized("invalid client".into()))?;

    let auth_code = match db::consume_authorization_code(&app.db, &code, &client_id, &redirect_uri)
        .await?
    {
        Some(ac) => ac,
        None => {
            if let Ok(Some((uid, cid))) = db::detect_code_reuse(&app.db, &code).await {
                tracing::warn!(user_id = %uid, client_id = %cid, "authorization code reuse detected -- revoking tokens");
                let _ = db::revoke_all_refresh_tokens_for_user_client(&app.db, &uid, &cid).await;
            }
            return Err(AppError::InvalidGrant(
                "invalid, expired, or already-used code".into(),
            ));
        }
    };

    // PKCE is mandatory here (there's no client_secret to fall back on).
    let challenge = auth_code.code_challenge.as_deref().ok_or_else(|| {
        AppError::BadRequest("this authorization code has no PKCE challenge".into())
    })?;
    let verifier = req
        .code_verifier
        .ok_or_else(|| AppError::BadRequest("code_verifier required".into()))?;
    let method = auth_code.code_challenge_method.as_deref().unwrap_or("S256");
    let ok = match method {
        "S256" => verify_pkce_s256(&verifier, challenge),
        "plain" => verifier == *challenge,
        _ => return Err(AppError::BadRequest("unknown code_challenge_method".into())),
    };
    if !ok {
        return Err(AppError::Unauthorized("PKCE verification failed".into()));
    }

    let user = db::get_user_by_id(&app.db, &auth_code.user_id)
        .await?
        .ok_or_else(|| AppError::Internal("user not found".into()))?;

    issue_tokens(
        app,
        &user,
        &client_id,
        &auth_code.scope,
        auth_code.nonce.as_deref(),
        Some(auth_code.auth_time),
    )
    .await
}

async fn token_refresh(app: &AppState, req: TokenRequest) -> Result<Json<TokenResponse>, AppError> {
    let token = req
        .refresh_token
        .ok_or_else(|| AppError::BadRequest("refresh_token required".into()))?;
    let client_id = req
        .client_id
        .ok_or_else(|| AppError::BadRequest("client_id required".into()))?;

    let rt = db::consume_refresh_token(&app.db, &token)
        .await?
        .ok_or_else(|| {
            AppError::InvalidGrant("invalid, expired, or revoked refresh_token".into())
        })?;
    if rt.client_id != client_id {
        return Err(AppError::InvalidGrant(
            "refresh_token belongs to a different client".into(),
        ));
    }

    metrics::refresh_rotation();

    let user = db::get_user_by_id(&app.db, &rt.user_id)
        .await?
        .ok_or_else(|| AppError::Internal("user not found".into()))?;

    issue_tokens(app, &user, &client_id, &rt.scope, None, Some(rt.auth_time)).await
}

async fn issue_tokens(
    app: &AppState,
    user: &db::User,
    client_id: &str,
    scope: &str,
    nonce: Option<&str>,
    auth_time: Option<i64>,
) -> Result<Json<TokenResponse>, AppError> {
    const ACCESS_TTL: i64 = 900; // 15 minutes
    const REFRESH_TTL_DAYS: i64 = 30;

    let client = db::get_client(&app.db, client_id)
        .await?
        .ok_or_else(|| AppError::Unauthorized("invalid client".into()))?;
    if client.access_restricted && !db::user_has_access(&app.db, &user.id, client_id).await? {
        return Err(AppError::InvalidGrant(
            "access to this app has been revoked".into(),
        ));
    }

    let roles = db::granted_roles(&app.db, &user.id, client_id).await?;

    let access_claims = Claims::new(
        &app.base_url,
        &user.id,
        client_id,
        ACCESS_TTL,
        None,
        &user.username,
        &user.display_name,
        scope,
        roles.clone(),
        None,
    );
    let access_token = encode_claims(app, &access_claims, &client.roles_claim)?;

    let id_token = if scope.split_whitespace().any(|s| s == "openid") {
        let id_claims = Claims::new(
            &app.base_url,
            &user.id,
            client_id,
            ACCESS_TTL,
            nonce,
            &user.username,
            &user.display_name,
            scope,
            roles,
            auth_time,
        );
        Some(encode_claims(app, &id_claims, &client.roles_claim)?)
    } else {
        None
    };

    let refresh_token = db::create_refresh_token(
        &app.db,
        &user.id,
        client_id,
        scope,
        REFRESH_TTL_DAYS,
        auth_time.unwrap_or(0),
    )
    .await?;

    metrics::token_issued("access");
    if id_token.is_some() {
        metrics::token_issued("id");
    }
    metrics::token_issued("refresh");

    Ok(Json(TokenResponse {
        access_token,
        token_type: "Bearer",
        expires_in: ACCESS_TTL,
        id_token,
        refresh_token: Some(refresh_token),
        scope: scope.to_string(),
    }))
}

/// Renames the `roles` field to `roles_claim` first if it's anything other
/// than the default -- lets an external relying party (e.g. ArgoCD, which
/// looks for group membership under a "groups" claim) consume the same
/// per-client role grants without the IDP needing any app-specific
/// knowledge beyond that one client's configured claim name.
fn rename_roles_claim(claims: &Claims, roles_claim: &str) -> serde_json::Value {
    let mut value = serde_json::to_value(claims).expect("Claims always serializes");
    if roles_claim != "roles" {
        if let Some(obj) = value.as_object_mut() {
            if let Some(roles) = obj.remove("roles") {
                obj.insert(roles_claim.to_string(), roles);
            }
        }
    }
    value
}

fn encode_claims(app: &AppState, claims: &Claims, roles_claim: &str) -> Result<String, AppError> {
    let value = rename_roles_claim(claims, roles_claim);
    Ok(jsonwebtoken::encode(
        &app.jwt_keys.signing_header(),
        &value,
        &app.jwt_keys.encoding,
    )?)
}

// ── Revocation endpoint (RFC 7009) ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RevokeRequest {
    pub token: String,
}

pub async fn revoke(
    State(app): State<AppState>,
    Form(req): Form<RevokeRequest>,
) -> impl IntoResponse {
    // Always 200, even for an unknown token -- RFC 7009 §2.2 (prevents
    // token enumeration).
    let _ = db::revoke_refresh_token(&app.db, &req.token).await;
    axum::http::StatusCode::OK
}

// ── UserInfo endpoint ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct UserInfoForm {
    pub access_token: Option<String>,
}

pub async fn userinfo(
    State(app): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let token = bearer_token(&headers)
        .ok_or_else(|| AppError::Unauthorized("missing bearer token".into()))?;
    userinfo_core(&app, &token).await
}

pub async fn userinfo_post(
    State(app): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<UserInfoForm>,
) -> Result<Json<serde_json::Value>, AppError> {
    let token = bearer_token(&headers)
        .or(form.access_token)
        .ok_or_else(|| AppError::Unauthorized("missing bearer token".into()))?;
    userinfo_core(&app, &token).await
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
}

async fn userinfo_core(app: &AppState, token: &str) -> Result<Json<serde_json::Value>, AppError> {
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.validate_aud = false;
    let data = jsonwebtoken::decode::<Claims>(token, &app.jwt_keys.decoding, &validation)?;

    let user = db::get_user_by_id(&app.db, &data.claims.sub)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    let mut body = serde_json::json!({ "sub": user.id, "roles": data.claims.roles });
    if data.claims.scope.split_whitespace().any(|s| s == "profile") {
        body["preferred_username"] = user.username.clone().into();
        body["name"] = user.display_name.clone().into();
    }
    Ok(Json(body))
}

// ── Session helpers (shared by passkey/admin routes) ──────────────────────────

pub async fn require_session(app: &AppState, jar: &PrivateCookieJar) -> Result<db::User, AppError> {
    let session_id = jar
        .get("session")
        .map(|c| c.value().to_string())
        .ok_or_else(|| AppError::Unauthorized("not logged in".into()))?;
    let session = db::get_session(&app.db, &session_id)
        .await?
        .ok_or_else(|| AppError::Unauthorized("session expired".into()))?;
    let user = db::get_user_by_id(&app.db, &session.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;
    Ok(user)
}

pub async fn require_admin(app: &AppState, jar: &PrivateCookieJar) -> Result<db::User, AppError> {
    let user = require_session(app, jar).await?;
    if !user.is_admin {
        return Err(AppError::Forbidden("admin required".into()));
    }
    Ok(user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encode_special_chars() {
        let encoded = url_encode("https://example.com/callback?foo=bar&baz=qux");
        assert!(!encoded.contains('?'));
        assert!(!encoded.contains('&'));
    }

    #[test]
    fn rename_roles_claim_leaves_default_untouched() {
        let claims = Claims::new(
            "https://idp.test",
            "u",
            "c",
            3600,
            None,
            "u",
            "U",
            "openid",
            vec!["admin".to_string()],
            None,
        );
        let value = rename_roles_claim(&claims, "roles");
        assert_eq!(value["roles"], serde_json::json!(["admin"]));
        assert!(value.get("groups").is_none());
    }

    #[test]
    fn rename_roles_claim_moves_to_custom_name() {
        let claims = Claims::new(
            "https://idp.test",
            "u",
            "c",
            3600,
            None,
            "u",
            "U",
            "openid",
            vec!["admin".to_string()],
            None,
        );
        let value = rename_roles_claim(&claims, "groups");
        assert_eq!(value["groups"], serde_json::json!(["admin"]));
        assert!(value.get("roles").is_none());
    }

    #[test]
    fn build_authorize_url_includes_params() {
        let params = AuthorizeQuery {
            response_type: Some("code".to_string()),
            client_id: Some("my-client".to_string()),
            redirect_uri: Some("https://app.example.com/callback".to_string()),
            scope: Some("openid profile".to_string()),
            state: Some("xyz".to_string()),
            nonce: Some("abc".to_string()),
            code_challenge: Some("challenge123".to_string()),
            code_challenge_method: Some("S256".to_string()),
            prompt: None,
            max_age: None,
        };
        let url = build_authorize_url(
            "https://idp.example.com",
            "my-client",
            "https://app.example.com/callback",
            &params,
            true,
        );
        assert!(url.contains("client_id=my-client"));
        assert!(url.contains("state=xyz"));
        assert!(url.contains("code_challenge=challenge123"));
    }
}
