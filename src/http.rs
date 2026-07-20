use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc};
use tracing::{debug, error};

use crate::config::Config;
use crate::errors::MCSError;
use crate::protocol::{JsonRpcRequest, JsonRpcResponse};

#[derive(Clone)]
pub struct HttpState {
    pub config: Arc<Config>,
}

pub async fn create_http_server(
    config: Arc<Config>,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let host = config.server.host.clone();
    let tls_cert = config.server.tls_cert.clone();
    let tls_key = config.server.tls_key.clone();
    let limit = config.server.max_http_body_bytes;
    let http_state = HttpState { config };

    let app = Router::new()
        .route("/rpc", axum::routing::post(handle_rpc))
        .route("/health", axum::routing::get(handle_health))
        .route("/version", axum::routing::get(handle_version))
        .route("/info", axum::routing::get(handle_info))
        .route("/tools", axum::routing::get(handle_tools))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(http_state);

    let socket_addr = resolve_addr(&host, port)?;

    if let (Some(cert), Some(key)) = (tls_cert, tls_key) {
        let tls = crate::tls::server_config(&cert, &key).await?;
        tracing::info!(%socket_addr, "HTTPS server listening");
        axum_server::bind_rustls(socket_addr, tls)
            .serve(app.into_make_service())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(socket_addr).await?;
        tracing::info!(%socket_addr, "HTTP server listening");
        axum::serve(listener, app).await?;
    }
    Ok(())
}

fn resolve_addr(host: &str, port: u16) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    use std::net::ToSocketAddrs;
    (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| format!("could not resolve bind address '{host}:{port}'").into())
}

async fn handle_rpc(State(state): State<HttpState>, headers: HeaderMap, body: Bytes) -> Response {
    if !is_json_content_type(&headers) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json",
        )
            .into_response();
    }
    if let Some(expected) = state.config.server.auth_token.as_deref() {
        let presented = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !crate::server::token_matches(presented, expected) {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    }
    let value = match serde_json::from_slice::<Value>(&body) {
        Ok(v) => v,
        Err(e) => {
            error!(error = %e, "HTTP JSON parse error");
            return Json(JsonRpcResponse::error(
                None,
                -32700,
                format!("Parse error: {e}"),
            ))
            .into_response();
        }
    };
    let req = match JsonRpcRequest::from_value(&value) {
        Ok(req) => req,
        Err(e) => {
            return Json(JsonRpcResponse::error(None, e.error_code(), e.to_string()))
                .into_response();
        }
    };
    let is_notification = req.id.is_none();
    debug!(method = %req.method, notification = is_notification, "HTTP RPC request");
    let response = crate::server::process_request_http(&req, &state.config).await;
    if is_notification {
        StatusCode::ACCEPTED.into_response()
    } else {
        Json(response).into_response()
    }
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
}

async fn handle_health() -> Json<Value> {
    Json(json!({ "status": "UP", "version": env!("CARGO_PKG_VERSION"), "transport": "http" }))
}

async fn handle_version() -> Json<Value> {
    Json(json!({ "version": env!("CARGO_PKG_VERSION") }))
}

async fn handle_info(State(state): State<HttpState>) -> Json<Value> {
    let categories: Vec<&str> = state
        .config
        .server
        .enabled_categories
        .iter()
        .map(|c| c.slug())
        .collect();
    let tools_count = crate::tools::ALL_TOOLS
        .iter()
        .filter(|t| state.config.server.enabled_categories.contains(&t.category))
        .count();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "protocolVersion": crate::server::LATEST_PROTOCOL_VERSION,
        "enabledToolCategories": categories,
        "enabledToolsCount": tools_count,
        "accessMode": state.config.server.access_mode,
        "allowedDirectories": state.config.allowed_directories
    }))
}

async fn handle_tools(State(state): State<HttpState>) -> Response {
    match serde_json::from_slice::<Value>(&state.config.tools_list_bytes) {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            let err = MCSError::JsonError(e);
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_addr;

    #[test]
    fn resolves_ipv4_loopback() {
        let addr = resolve_addr("127.0.0.1", 3001).unwrap();
        assert_eq!(addr.to_string(), "127.0.0.1:3001");
    }

    #[test]
    fn resolves_ipv6_loopback() {
        let addr = resolve_addr("::1", 3001).unwrap();
        assert_eq!(addr.to_string(), "[::1]:3001");
    }

    #[test]
    fn resolves_ipv6_unspecified() {
        let addr = resolve_addr("::", 3001).unwrap();
        assert_eq!(addr.to_string(), "[::]:3001");
    }
}
