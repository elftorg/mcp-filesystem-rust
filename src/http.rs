use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::stream;
use serde_json::{Value, json};
use std::{
    collections::HashSet, convert::Infallible, net::SocketAddr, sync::Arc, time::Duration,
};
use tokio::sync::RwLock;
use tracing::{debug, error};
use uuid::Uuid;

use crate::config::Config;
use crate::errors::MCSError;
use crate::protocol::{JsonRpcRequest, JsonRpcResponse};

const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";

#[derive(Clone)]
pub struct HttpState {
    pub config: Arc<Config>,
    sessions: Arc<RwLock<HashSet<String>>>,
    allowed_origins: Arc<Vec<String>>,
}

pub async fn create_http_server(
    config: Arc<Config>,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let host = config.server.host.clone();
    let tls_cert = config.server.tls_cert.clone();
    let tls_key = config.server.tls_key.clone();
    let limit = config.server.max_http_body_bytes;
    let allowed_origins = std::env::var("MCP_ALLOWED_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();

    let state = HttpState {
        config,
        sessions: Arc::new(RwLock::new(HashSet::new())),
        allowed_origins: Arc::new(allowed_origins),
    };

    let app = Router::new()
        .route("/rpc", axum::routing::post(handle_rpc))
        .route(
            "/mcp",
            axum::routing::get(handle_mcp_get)
                .post(handle_mcp_post)
                .delete(handle_mcp_delete),
        )
        .route("/health", axum::routing::get(handle_health))
        .route("/version", axum::routing::get(handle_version))
        .route("/info", axum::routing::get(handle_info))
        .route("/tools", axum::routing::get(handle_tools))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(state);

    let socket_addr = resolve_addr(&host, port)?;
    if let (Some(cert), Some(key)) = (tls_cert, tls_key) {
        let tls = crate::tls::server_config(&cert, &key).await?;
        tracing::info!(%socket_addr, "HTTPS MCP App Server listening");
        axum_server::bind_rustls(socket_addr, tls)
            .serve(app.into_make_service())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(socket_addr).await?;
        tracing::info!(%socket_addr, "HTTP MCP App Server listening");
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
    if let Some(response) = common_request_rejection(&state, &headers, true) {
        return response;
    }

    let request = match parse_json_rpc(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let notification = request.id.is_none();
    debug!(method = %request.method, notification, transport = "rpc", "HTTP request");
    let response = crate::server::process_request_http(&request, &state.config).await;
    if notification {
        StatusCode::ACCEPTED.into_response()
    } else {
        Json(response).into_response()
    }
}

async fn handle_mcp_post(
    State(state): State<HttpState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(response) = common_request_rejection(&state, &headers, true) {
        return response;
    }
    if let Some(response) = reject_protocol_version(&headers) {
        return response;
    }

    let accepts_json = accepts(&headers, "application/json");
    let accepts_sse = accepts(&headers, "text/event-stream");
    if !accepts_json && !accepts_sse {
        return (
            StatusCode::NOT_ACCEPTABLE,
            "Accept must include application/json or text/event-stream",
        )
            .into_response();
    }

    let request = match parse_json_rpc(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let notification = request.id.is_none();
    let initializing = request.method == "initialize";

    let session_id = if initializing {
        let session = Uuid::new_v4().to_string();
        state.sessions.write().await.insert(session.clone());
        session
    } else {
        match validate_session(&state, &headers).await {
            Ok(session) => session,
            Err(response) => return response,
        }
    };

    debug!(
        method = %request.method,
        notification,
        transport = "streamable-http",
        session_id = %session_id,
        "MCP request"
    );
    let rpc_response = crate::server::process_request_http(&request, &state.config).await;

    if notification {
        return with_session(StatusCode::ACCEPTED.into_response(), &session_id);
    }

    if accepts_sse && !accepts_json {
        let payload = match serde_json::to_string(&rpc_response) {
            Ok(payload) => payload,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Cannot serialize JSON-RPC response: {error}"),
                )
                    .into_response();
            }
        };
        let events = stream::once(async move {
            Ok::<Event, Infallible>(Event::default().event("message").data(payload))
        });
        return with_session(Sse::new(events).into_response(), &session_id);
    }

    with_session(Json(rpc_response).into_response(), &session_id)
}

async fn handle_mcp_get(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Some(response) = common_request_rejection(&state, &headers, false) {
        return response;
    }
    if let Some(response) = reject_protocol_version(&headers) {
        return response;
    }
    if !accepts(&headers, "text/event-stream") {
        return (
            StatusCode::NOT_ACCEPTABLE,
            "Accept must include text/event-stream",
        )
            .into_response();
    }

    let session_id = match validate_session(&state, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };

    let events = stream::pending::<Result<Event, Infallible>>();
    let response = Sse::new(events)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("mcp keepalive"),
        )
        .into_response();
    with_session(response, &session_id)
}

async fn handle_mcp_delete(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Some(response) = common_request_rejection(&state, &headers, false) {
        return response;
    }
    let session_id = match session_header(&headers) {
        Some(value) => value,
        None => return (StatusCode::BAD_REQUEST, "Missing MCP-Session-Id").into_response(),
    };

    if state.sessions.write().await.remove(&session_id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "Unknown MCP session").into_response()
    }
}

fn common_request_rejection(
    state: &HttpState,
    headers: &HeaderMap,
    require_json: bool,
) -> Option<Response> {
    if require_json && !is_json_content_type(headers) {
        return Some(
            (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Content-Type must be application/json",
            )
                .into_response(),
        );
    }
    if !origin_allowed(headers, &state.allowed_origins) {
        return Some((StatusCode::FORBIDDEN, "Origin is not allowed").into_response());
    }
    if let Some(expected) = state.config.server.auth_token.as_deref() {
        let presented = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if !crate::server::token_matches(presented, expected) {
            let mut response = (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            return Some(response);
        }
    }
    None
}

fn parse_json_rpc(body: &[u8]) -> Result<JsonRpcRequest, Response> {
    let value = serde_json::from_slice::<Value>(body).map_err(|error| {
        error!(%error, "HTTP JSON parse error");
        Json(JsonRpcResponse::error(
            None,
            -32700,
            format!("Parse error: {error}"),
        ))
        .into_response()
    })?;

    JsonRpcRequest::from_value(&value).map_err(|error| {
        Json(JsonRpcResponse::error(
            None,
            error.error_code(),
            error.to_string(),
        ))
        .into_response()
    })
}

async fn validate_session(state: &HttpState, headers: &HeaderMap) -> Result<String, Response> {
    let session = session_header(headers)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "Missing MCP-Session-Id").into_response())?;
    if state.sessions.read().await.contains(&session) {
        Ok(session)
    } else {
        Err((StatusCode::NOT_FOUND, "Unknown or expired MCP session").into_response())
    }
}

fn session_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get(HeaderName::from_static(SESSION_HEADER))
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn with_session(mut response: Response, session_id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(session_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(SESSION_HEADER), value);
    }
    response
}

fn reject_protocol_version(headers: &HeaderMap) -> Option<Response> {
    let version = headers
        .get(HeaderName::from_static(PROTOCOL_HEADER))
        .and_then(|value| value.to_str().ok())?;
    if crate::server::is_supported_protocol_version(version) {
        None
    } else {
        Some(
            (
                StatusCode::BAD_REQUEST,
                format!("Unsupported MCP-Protocol-Version: {version}"),
            )
                .into_response(),
        )
    }
}

fn origin_allowed(headers: &HeaderMap, configured: &[String]) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };
    if origin == "null" {
        return false;
    }
    if configured.iter().any(|allowed| allowed == origin) {
        return true;
    }

    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    origin
        .split_once("://")
        .map(|(_, authority)| authority.eq_ignore_ascii_case(host))
        .unwrap_or(false)
}

fn accepts(headers: &HeaderMap, mime: &str) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|item| {
                let media_type = item.split(';').next().unwrap_or_default().trim();
                media_type == mime || media_type == "*/*"
            })
        })
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"))
}

async fn handle_health() -> Json<Value> {
    Json(json!({
        "status": "UP",
        "version": env!("CARGO_PKG_VERSION"),
        "transport": "streamable-http",
        "mcpEndpoint": "/mcp",
        "legacyEndpoint": "/rpc"
    }))
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
        .map(|category| category.slug())
        .collect();
    let tool_count = crate::tools::ALL_TOOLS
        .iter()
        .filter(|tool| {
            state
                .config
                .server
                .enabled_categories
                .contains(&tool.category)
        })
        .count();

    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "protocolVersion": crate::server::LATEST_PROTOCOL_VERSION,
        "transports": ["stdio", "streamable-http", "legacy-http-json-rpc"],
        "mcpEndpoint": "/mcp",
        "enabledToolCategories": categories,
        "enabledToolsCount": tool_count,
        "accessMode": state.config.server.access_mode,
        "allowedDirectories": state.config.allowed_directories,
        "activeSessions": state.sessions.read().await.len()
    }))
}

async fn handle_tools(State(state): State<HttpState>) -> Response {
    match serde_json::from_slice::<Value>(&state.config.tools_list_bytes) {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            let error = MCSError::JsonError(error);
            (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_ipv4_and_ipv6() {
        assert_eq!(
            resolve_addr("127.0.0.1", 3001).unwrap().to_string(),
            "127.0.0.1:3001"
        );
        assert_eq!(
            resolve_addr("::1", 3001).unwrap().to_string(),
            "[::1]:3001"
        );
    }

    #[test]
    fn validates_same_origin_or_allowlist() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("example.com"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://example.com"),
        );
        assert!(origin_allowed(&headers, &[]));

        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://apps.example.net"),
        );
        assert!(origin_allowed(
            &headers,
            &["https://apps.example.net".into()]
        ));
        assert!(!origin_allowed(&headers, &[]));
    }

    #[test]
    fn parses_accept_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        assert!(accepts(&headers, "application/json"));
        assert!(accepts(&headers, "text/event-stream"));
    }

    #[test]
    fn rejects_unknown_protocol_versions() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static(PROTOCOL_HEADER),
            HeaderValue::from_static("1900-01-01"),
        );
        assert!(reject_protocol_version(&headers).is_some());
    }
}
