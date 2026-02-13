// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! HTTP authentication module for trx-frontend-http.
//!
//! Provides optional session-based authentication with two roles:
//! - `Rx`: read-only access to status/events/audio
//! - `Control`: full access including TX/PTT control

use actix_web::{
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    get, post, web, Error, HttpRequest, HttpResponse, Responder, cookie::Cookie,
};
use futures_util::future::LocalBoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};


/// Unique session identifier (hex-encoded 128-bit random)
pub type SessionId = String;

/// Authentication role
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthRole {
    /// Read-only access (rx passphrase)
    Rx,
    /// Full control access (control passphrase)
    Control,
}

impl AuthRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rx => "rx",
            Self::Control => "control",
        }
    }
}

/// Session record stored in the session store
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub role: AuthRole,
    pub issued_at: SystemTime,
    pub expires_at: SystemTime,
    pub last_seen: SystemTime,
}

impl SessionRecord {
    pub fn is_expired(&self) -> bool {
        SystemTime::now() > self.expires_at
    }

    pub fn update_last_seen(&mut self) {
        self.last_seen = SystemTime::now();
    }
}

/// Thread-safe in-memory session store
#[derive(Clone)]
pub struct SessionStore {
    sessions: Arc<RwLock<HashMap<SessionId, SessionRecord>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a new session with the given role and TTL
    pub fn create(&self, role: AuthRole, ttl: Duration) -> SessionId {
        let now = SystemTime::now();
        let expires_at = now + ttl;
        let session_id = Self::generate_session_id();

        let record = SessionRecord {
            role,
            issued_at: now,
            expires_at,
            last_seen: now,
        };

        let mut store = self.sessions.write().unwrap();
        store.insert(session_id.clone(), record);
        session_id
    }

    /// Get session by ID (returns None if expired or not found)
    pub fn get(&self, session_id: &SessionId) -> Option<SessionRecord> {
        let mut store = self.sessions.write().unwrap();
        if let Some(record) = store.get_mut(session_id) {
            if !record.is_expired() {
                record.update_last_seen();
                return Some(record.clone());
            } else {
                store.remove(session_id);
            }
        }
        None
    }

    /// Invalidate a session
    pub fn remove(&self, session_id: &SessionId) {
        let mut store = self.sessions.write().unwrap();
        store.remove(session_id);
    }

    /// Remove all expired sessions
    pub fn cleanup_expired(&self) {
        let mut store = self.sessions.write().unwrap();
        let now = SystemTime::now();
        store.retain(|_, record| record.expires_at > now);
    }

    /// Generate a new random session ID (128-bit, hex-encoded)
    fn generate_session_id() -> SessionId {
        let random_bytes = rand::random::<[u8; 16]>();
        hex::encode(random_bytes)
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Cookie SameSite attribute
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SameSite {
    Strict,
    #[default]
    Lax,
    None,
}

impl SameSite {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Strict => "Strict",
            Self::Lax => "Lax",
            Self::None => "None",
        }
    }
}

/// Runtime authentication configuration
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub enabled: bool,
    pub rx_passphrase: Option<String>,
    pub control_passphrase: Option<String>,
    pub tx_access_control_enabled: bool,
    pub session_ttl: Duration,
    pub cookie_secure: bool,
    pub cookie_same_site: SameSite,
}

impl AuthConfig {
    /// Create a new auth config with all fields
    pub fn new(
        enabled: bool,
        rx_passphrase: Option<String>,
        control_passphrase: Option<String>,
        tx_access_control_enabled: bool,
        session_ttl: Duration,
        cookie_secure: bool,
        cookie_same_site: SameSite,
    ) -> Self {
        Self {
            enabled,
            rx_passphrase,
            control_passphrase,
            tx_access_control_enabled,
            session_ttl,
            cookie_secure,
            cookie_same_site,
        }
    }

    /// Check passphrase and return the corresponding role
    pub fn check_passphrase(&self, passphrase: &str) -> Option<AuthRole> {
        // Use constant-time comparison to reduce timing attacks
        if let Some(ctrl_pass) = &self.control_passphrase {
            if constant_time_eq(passphrase, ctrl_pass) {
                return Some(AuthRole::Control);
            }
        }
        if let Some(rx_pass) = &self.rx_passphrase {
            if constant_time_eq(passphrase, rx_pass) {
                return Some(AuthRole::Rx);
            }
        }
        None
    }
}

/// Application data for authentication
pub struct AuthState {
    pub config: AuthConfig,
    pub store: SessionStore,
}

impl AuthState {
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config,
            store: SessionStore::new(),
        }
    }
}

/// Constant-time string comparison to mitigate timing attacks
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    if a_bytes.len() != b_bytes.len() {
        return false;
    }

    let mut result = 0u8;
    for (x, y) in a_bytes.iter().zip(b_bytes.iter()) {
        result |= x ^ y;
    }
    result == 0
}

/// Login request body
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub passphrase: String,
}

/// Session status response
#[derive(Debug, Serialize)]
pub struct SessionStatus {
    pub authenticated: bool,
    pub role: Option<String>,
}

/// Login response
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub authenticated: bool,
    pub role: String,
}

/// Extract session from cookie
fn extract_session_id(req: &HttpRequest) -> Option<SessionId> {
    req.cookie("trx_http_sid")
        .map(|cookie| cookie.value().to_string())
}

/// Get session from request, return role if valid
pub fn get_session_role(req: &HttpRequest, auth_state: &AuthState) -> Option<AuthRole> {
    let session_id = extract_session_id(req)?;
    let record = auth_state.store.get(&session_id)?;
    Some(record.role)
}

// ============================================================================
// Endpoints
// ============================================================================

/// POST /auth/login
#[post("/auth/login")]
pub async fn login(
    _req: HttpRequest,
    body: web::Json<LoginRequest>,
    auth_state: web::Data<AuthState>,
) -> Result<impl Responder, Error> {
    if !auth_state.config.enabled {
        return Ok(HttpResponse::NotFound().finish());
    }

    // Check passphrase
    let role = match auth_state.config.check_passphrase(&body.passphrase) {
        Some(r) => r,
        None => {
            return Ok(HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "Invalid credentials"
            })));
        }
    };

    // Create session
    let session_id = auth_state.store.create(role, auth_state.config.session_ttl);

    let mut cookie = Cookie::new("trx_http_sid", session_id);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_secure(auth_state.config.cookie_secure);

    // Set SameSite attribute
    match auth_state.config.cookie_same_site {
        SameSite::Strict => cookie.set_same_site(actix_web::cookie::SameSite::Strict),
        SameSite::Lax => cookie.set_same_site(actix_web::cookie::SameSite::Lax),
        SameSite::None => cookie.set_same_site(actix_web::cookie::SameSite::None),
    };

    // Convert Duration to cookie time::Duration
    let ttl_secs = auth_state.config.session_ttl.as_secs() as i64;
    cookie.set_max_age(actix_web::cookie::time::Duration::seconds(ttl_secs));

    Ok(HttpResponse::Ok()
        .cookie(cookie)
        .json(LoginResponse {
            authenticated: true,
            role: role.as_str().to_string(),
        }))
}

/// POST /auth/logout
#[post("/auth/logout")]
pub async fn logout(
    req: HttpRequest,
    auth_state: web::Data<AuthState>,
) -> Result<impl Responder, Error> {
    if !auth_state.config.enabled {
        return Ok(HttpResponse::NotFound().finish());
    }

    // Invalidate session
    if let Some(session_id) = extract_session_id(&req) {
        auth_state.store.remove(&session_id);
    }

    // Clear cookie by setting max_age to 0
    let mut cookie = Cookie::new("trx_http_sid", "");
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_max_age(actix_web::cookie::time::Duration::seconds(0));

    Ok(HttpResponse::Ok()
        .cookie(cookie)
        .json(serde_json::json!({
            "logged_out": true
        })))
}

/// GET /auth/session
#[get("/auth/session")]
pub async fn session_status(
    req: HttpRequest,
    auth_state: web::Data<AuthState>,
) -> Result<impl Responder, Error> {
    // If auth is disabled, grant full control access without requiring login
    if !auth_state.config.enabled {
        return Ok(HttpResponse::Ok().json(SessionStatus {
            authenticated: true,
            role: Some("control".to_string()),
        }));
    }

    let session_id = extract_session_id(&req);
    let role = session_id
        .and_then(|sid| auth_state.store.get(&sid))
        .map(|r| r.role.as_str().to_string());

    Ok(HttpResponse::Ok().json(SessionStatus {
        authenticated: role.is_some(),
        role,
    }))
}

// ============================================================================
// Middleware
// ============================================================================

/// Route classification for access control
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteAccess {
    /// Publicly accessible (no auth required)
    Public,
    /// Read-only (rx or control role required)
    Read,
    /// Control only (control role required)
    Control,
}

impl RouteAccess {
    /// Classify a request path
    fn from_path(path: &str) -> Self {
        // Public routes
        if path == "/" || path == "/index.html" || path.starts_with("/auth/") {
            return Self::Public;
        }

        // Static assets
        if path.starts_with("/style.css")
            || path.starts_with("/app.js")
            || path.ends_with(".js")
            || path.ends_with(".css")
            || path.ends_with(".png")
            || path.ends_with(".jpg")
            || path.ends_with(".gif")
            || path.ends_with(".svg")
            || path.ends_with(".favicon")
            || path.ends_with(".ico")
        {
            return Self::Public;
        }

        // Read-only routes
        if path == "/status"
            || path == "/events"
            || path == "/decode"
            || path == "/audio"
            || path.starts_with("/status?")
            || path.starts_with("/events?")
            || path.starts_with("/decode?")
            || path.starts_with("/audio?")
        {
            return Self::Read;
        }

        // All other routes require control
        Self::Control
    }

    fn allows(&self, role: Option<AuthRole>) -> bool {
        match self {
            Self::Public => true,
            Self::Read => role.is_some(),
            Self::Control => matches!(role, Some(AuthRole::Control)),
        }
    }
}

/// Authentication middleware
pub struct AuthMiddleware;

impl<S, B> Transform<S, ServiceRequest> for AuthMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = AuthMiddlewareService<S>;
    type Future = std::future::Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        std::future::ready(Ok(AuthMiddlewareService { service }))
    }
}

pub struct AuthMiddlewareService<S> {
    service: S,
}

impl<S, B> Service<ServiceRequest> for AuthMiddlewareService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let path = req.path().to_string();
        let access = RouteAccess::from_path(&path);

        // If route is public, allow unconditionally
        if access == RouteAccess::Public {
            let fut = self.service.call(req);
            return Box::pin(async move {
                let res = fut.await?;
                Ok(res)
            });
        }

        // For protected routes, check auth
        let auth_state = req
            .app_data::<web::Data<AuthState>>()
            .cloned();

        if let Some(auth_state) = auth_state {
            if !auth_state.config.enabled {
                // Auth disabled - allow all
                let fut = self.service.call(req);
                return Box::pin(async move {
                    let res = fut.await?;
                    Ok(res)
                });
            }

            // Auth enabled - check role
            let role = get_session_role(req.request(), &auth_state);

            if !access.allows(role) {
                // Access denied - return 401/403
                return Box::pin(async move {
                    Err(actix_web::error::ErrorUnauthorized(
                        "Unauthorized".to_string(),
                    ))
                });
            }
        }

        let fut = self.service.call(req);
        Box::pin(async move {
            let res = fut.await?;
            Ok(res)
        })
    }
}

/// Check if a path is a TX/PTT endpoint (for future TX access control)
#[allow(dead_code)]
fn is_tx_endpoint(path: &str) -> bool {
    path.contains("ptt")
        || path.contains("set_ptt")
        || path.contains("toggle_ptt")
        || path.contains("set_tx")
        || path.contains("toggle_tx")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_route_access_public_paths() {
        assert_eq!(RouteAccess::from_path("/"), RouteAccess::Public);
        assert_eq!(RouteAccess::from_path("/auth/login"), RouteAccess::Public);
        assert_eq!(RouteAccess::from_path("/auth/logout"), RouteAccess::Public);
        assert_eq!(RouteAccess::from_path("/style.css"), RouteAccess::Public);
        assert_eq!(RouteAccess::from_path("/app.js"), RouteAccess::Public);
    }

    #[test]
    fn test_route_access_read_paths() {
        assert_eq!(RouteAccess::from_path("/status"), RouteAccess::Read);
        assert_eq!(RouteAccess::from_path("/events"), RouteAccess::Read);
        assert_eq!(RouteAccess::from_path("/decode"), RouteAccess::Read);
        assert_eq!(RouteAccess::from_path("/audio"), RouteAccess::Read);
    }

    #[test]
    fn test_route_access_control_paths() {
        assert_eq!(
            RouteAccess::from_path("/set_freq"),
            RouteAccess::Control
        );
        assert_eq!(
            RouteAccess::from_path("/set_mode"),
            RouteAccess::Control
        );
    }

    #[test]
    fn test_route_access_allows() {
        assert!(RouteAccess::Public.allows(None));
        assert!(RouteAccess::Public.allows(Some(AuthRole::Rx)));
        assert!(RouteAccess::Public.allows(Some(AuthRole::Control)));

        assert!(!RouteAccess::Read.allows(None));
        assert!(RouteAccess::Read.allows(Some(AuthRole::Rx)));
        assert!(RouteAccess::Read.allows(Some(AuthRole::Control)));

        assert!(!RouteAccess::Control.allows(None));
        assert!(!RouteAccess::Control.allows(Some(AuthRole::Rx)));
        assert!(RouteAccess::Control.allows(Some(AuthRole::Control)));
    }

    #[test]
    fn test_session_store_create_and_get() {
        let store = SessionStore::new();
        let ttl = Duration::from_secs(3600);
        let session_id = store.create(AuthRole::Rx, ttl);

        let record = store.get(&session_id);
        assert!(record.is_some());
        let record = record.unwrap();
        assert_eq!(record.role, AuthRole::Rx);
        assert!(!record.is_expired());
    }

    #[test]
    fn test_session_store_remove() {
        let store = SessionStore::new();
        let ttl = Duration::from_secs(3600);
        let session_id = store.create(AuthRole::Rx, ttl);

        store.remove(&session_id);
        assert!(store.get(&session_id).is_none());
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq("test", "test"));
        assert!(!constant_time_eq("test", "fail"));
        assert!(!constant_time_eq("test", "test2"));
        assert!(!constant_time_eq("", "test"));
    }

    #[test]
    fn test_auth_config_check_passphrase_control() {
        let config = AuthConfig {
            enabled: true,
            rx_passphrase: None,
            control_passphrase: Some("ctrl-pass".to_string()),
            tx_access_control_enabled: true,
            session_ttl: Duration::from_secs(3600),
            cookie_secure: false,
            cookie_same_site: SameSite::Lax,
        };

        assert_eq!(
            config.check_passphrase("ctrl-pass"),
            Some(AuthRole::Control)
        );
        assert_eq!(config.check_passphrase("wrong"), None);
    }

    #[test]
    fn test_auth_config_check_passphrase_rx() {
        let config = AuthConfig {
            enabled: true,
            rx_passphrase: Some("rx-pass".to_string()),
            control_passphrase: None,
            tx_access_control_enabled: true,
            session_ttl: Duration::from_secs(3600),
            cookie_secure: false,
            cookie_same_site: SameSite::Lax,
        };

        assert_eq!(config.check_passphrase("rx-pass"), Some(AuthRole::Rx));
        assert_eq!(config.check_passphrase("wrong"), None);
    }

    #[test]
    fn test_auth_config_check_passphrase_both() {
        let config = AuthConfig {
            enabled: true,
            rx_passphrase: Some("rx-pass".to_string()),
            control_passphrase: Some("ctrl-pass".to_string()),
            tx_access_control_enabled: true,
            session_ttl: Duration::from_secs(3600),
            cookie_secure: false,
            cookie_same_site: SameSite::Lax,
        };

        // Control is checked first
        assert_eq!(
            config.check_passphrase("ctrl-pass"),
            Some(AuthRole::Control)
        );
        assert_eq!(config.check_passphrase("rx-pass"), Some(AuthRole::Rx));
        assert_eq!(config.check_passphrase("wrong"), None);
    }

    #[test]
    fn test_is_tx_endpoint() {
        assert!(is_tx_endpoint("/set_ptt"));
        assert!(is_tx_endpoint("/toggle_ptt"));
        assert!(is_tx_endpoint("/set_tx"));
        assert!(!is_tx_endpoint("/status"));
    }
}
