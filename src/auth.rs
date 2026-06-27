//! API key authentication middleware.
//!
//! When `PROXY_AUTH_TOKENS` is set, all endpoints except `/health` require
//! a valid `x-api-key` header. When unset or empty, no authentication is
//! performed (backward compatible).
//!
//! ## Usage
//! ```ignore
//! App::new()
//!     .wrap(Auth::new(config.auth.clone()))
//!     ...
//! ```

use std::pin::Pin;

use actix_web::{
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    Error, HttpResponse,
};

/// Authentication configuration.
#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
    /// Valid API tokens. Empty = no auth required.
    pub tokens: Vec<String>,
}

impl AuthConfig {
    /// Returns true if auth is enabled (at least one token configured).
    pub fn is_enabled(&self) -> bool {
        !self.tokens.is_empty()
    }

    /// Validate an `x-api-key` header value.
    pub fn is_valid(&self, api_key: &str) -> bool {
        self.tokens.iter().any(|t| t == api_key)
    }
}

// ── Middleware ──────────────────────────────────────────────────────────

/// Actix-web middleware that validates `x-api-key` on every request
/// except `/health`.
pub struct Auth {
    config: AuthConfig,
}

impl Auth {
    pub fn new(config: AuthConfig) -> Self {
        Auth { config }
    }
}

impl<S, B> Transform<S, ServiceRequest> for Auth
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Transform = AuthMiddleware<S>;
    type InitError = ();
    type Future = std::future::Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        std::future::ready(Ok(AuthMiddleware {
            service,
            config: self.config.clone(),
        }))
    }
}

pub struct AuthMiddleware<S> {
    service: S,
    config: AuthConfig,
}

impl<S, B> Service<ServiceRequest> for AuthMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>>>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        // No auth configured → pass through
        if !self.config.is_enabled() {
            return Box::pin(self.service.call(req));
        }

        // Health check is always public
        if req.path() == "/health" {
            return Box::pin(self.service.call(req));
        }

        // Validate header
        let api_key = req
            .headers()
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if self.config.is_valid(api_key) {
            return Box::pin(self.service.call(req));
        }

        // Return 401 via InternalError wrapping the HttpResponse
        let res = HttpResponse::Unauthorized()
            .json(serde_json::json!({
                "error": {
                    "type": "authentication_error",
                    "message": "invalid or missing x-api-key. Set PROXY_AUTH_TOKENS or provide a valid key via the x-api-key header."
                }
            }));
        let err = actix_web::error::InternalError::from_response("", res);
        Box::pin(std::future::ready(Err(err.into())))
    }
}
