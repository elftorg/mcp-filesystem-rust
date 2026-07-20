use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::error;

use crate::actions;
use crate::config::Config;
use crate::errors::{MCSError, Result as MCSResult};
use crate::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::resources::{self, UI_RESOURCE_URI};
use std::sync::{Arc, LazyLock};

static ALL_TOOL_DEFS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../tools.json")).expect("Failed to parse tools.json")
});

pub fn build_tools_list_response(enabled: &[crate::tools::ToolCategory]) -> Vec<u8> {
    let tools: Vec<Value> = ALL_TOOL_DEFS
        .iter()
        .filter(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| crate::tools::is_tool_available(name, enabled))
        })
        .map(enhance_tool_descriptor)
        .collect();

    serde_json::to_vec(&json!({ "tools": tools }))
        .expect("Failed to serialize tools/list response")
}

fn enhance_tool_descriptor(tool: &Value) -> Value {
    let mut tool = tool.clone();
    let Some(object) = tool.as_object_mut() else {
        return tool;
    };
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    object
        .entry("title")
        .or_insert_with(|| Value::String(human_title(&name)));
    object
        .entry("outputSchema")
        .or_insert_with(|| json!({ "type": "object", "additionalProperties": true }));

    let read_only = name.starts_with("read")
        || name.starts_with("list")
        || name.starts_with("search")
        || name.starts_with("grep")
        || matches!(
            name.as_str(),
            "get_file_info"
                | "get_disk_usage"
                | "hash_file"
                | "directory_tree"
                | "generate_key"
                | "csv_read"
                | "csv_read_column_values_range"
                | "csv_read_row_range"
                | "csv_select_column_range"
        );
    let destructive = matches!(
        name.as_str(),
        "write_file"
            | "edit_file"
            | "move_file"
            | "delete_file"
            | "delete_directory"
            | "set_permissions"
            | "decompress_tar"
            | "csv_update_cell"
            | "csv_remove_row"
            | "csv_remove_column"
            | "csv_rename_column"
    );
    let idempotent = matches!(
        name.as_str(),
        "write_file" | "create_directory" | "set_permissions"
    );

    let annotations = object.entry("annotations").or_insert_with(|| json!({}));
    if let Some(annotations) = annotations.as_object_mut() {
        annotations
            .entry("readOnlyHint")
            .or_insert(Value::Bool(read_only));
        annotations
            .entry("openWorldHint")
            .or_insert(Value::Bool(false));
        annotations
            .entry("destructiveHint")
            .or_insert(Value::Bool(destructive));
        annotations
            .entry("idempotentHint")
            .or_insert(Value::Bool(idempotent));
    }

    if has_browser_ui(&name) {
        merge_app_meta(object);
    }

    tool
}

fn merge_app_meta(object: &mut Map<String, Value>) {
    let meta = object.entry("_meta").or_insert_with(|| json!({}));
    let Some(meta) = meta.as_object_mut() else {
        return;
    };
    meta.insert(
        "ui".into(),
        json!({
            "resourceUri": UI_RESOURCE_URI,
            "visibility": ["model", "app"]
        }),
    );
    meta.insert(
        "openai/outputTemplate".into(),
        Value::String(UI_RESOURCE_URI.into()),
    );
    meta.insert(
        "openai/toolInvocation/invoking".into(),
        Value::String("Reading the filesystem…".into()),
    );
    meta.insert(
        "openai/toolInvocation/invoked".into(),
        Value::String("Filesystem view ready.".into()),
    );
}

fn has_browser_ui(name: &str) -> bool {
    matches!(
        name,
        "list_allowed_directories"
            | "list_directory"
            | "list_directory_with_sizes"
            | "directory_tree"
            | "search_files"
            | "read_text_file"
            | "read_media_file"
            | "csv_read"
    )
}

fn human_title(name: &str) -> String {
    name.split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const BUFFER_CAPACITY: usize = 65_536;
const NEWLINE: &[u8] = b"\n";

enum LineRead {
    Line,
    Eof,
    TooLong,
}

async fn read_line_capped<R>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    output: &mut String,
    max: usize,
) -> std::io::Result<LineRead>
where
    R: AsyncBufReadExt + Unpin,
{
    buffer.clear();
    output.clear();

    fn finish(buffer: &[u8], output: &mut String) -> LineRead {
        match std::str::from_utf8(buffer) {
            Ok(value) => output.push_str(value),
            Err(_) => output.push_str(&String::from_utf8_lossy(buffer)),
        }
        LineRead::Line
    }

    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if buffer.is_empty() {
                Ok(LineRead::Eof)
            } else {
                Ok(finish(buffer, output))
            };
        }

        match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => {
                if buffer.len() + index + 1 > max {
                    reader.consume(index + 1);
                    return Ok(LineRead::TooLong);
                }
                buffer.extend_from_slice(&available[..=index]);
                reader.consume(index + 1);
                return Ok(finish(buffer, output));
            }
            None => {
                let length = available.len();
                if buffer.len() + length > max {
                    reader.consume(length);
                    return Ok(LineRead::TooLong);
                }
                buffer.extend_from_slice(available);
                reader.consume(length);
            }
        }
    }
}

pub fn token_matches(presented: &str, expected: &str) -> bool {
    let presented = presented.trim();
    let presented = presented
        .strip_prefix("Bearer ")
        .unwrap_or(presented)
        .trim();
    let presented_hash = Sha256::digest(presented.as_bytes());
    let expected_hash = Sha256::digest(expected.as_bytes());
    presented_hash.ct_eq(&expected_hash).into()
}

fn parse_error(message: String) -> JsonRpcResponse {
    let error = MCSError::ParseError(message);
    JsonRpcResponse::error(None, error.error_code(), error.to_string())
}

fn parse_request(line: &str) -> std::result::Result<JsonRpcRequest, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err("Empty request".into());
    }
    serde_json::from_str::<Value>(trimmed)
        .map_err(|error| MCSError::ParseError(error.to_string()).to_string())
        .and_then(|value| JsonRpcRequest::from_value(&value).map_err(|error| error.to_string()))
}

pub struct MCPServer {
    config: Arc<Config>,
}

impl MCPServer {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    pub const fn from_arc(config: Arc<Config>) -> Self {
        Self { config }
    }

    pub async fn run_stdio(&self) -> MCSResult<()> {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::with_capacity(BUFFER_CAPACITY, stdin);
        let mut stdout = tokio::io::stdout();
        let mut line = String::with_capacity(1_024);
        let mut read_buffer = Vec::with_capacity(1_024);
        let mut response_buffer = Vec::with_capacity(65_536);
        let max = self.config.server.max_request_bytes;

        loop {
            match read_line_capped(&mut reader, &mut read_buffer, &mut line, max).await {
                Ok(LineRead::Eof) => break,
                Ok(LineRead::Line) => {
                    process_one_line(&line, &self.config, &mut response_buffer, &mut stdout).await?;
                }
                Ok(LineRead::TooLong) => {
                    write_oversize_error(&mut response_buffer, &mut stdout, max).await?;
                    break;
                }
                Err(error) => {
                    error!("IO error: {error}");
                    break;
                }
            }
        }
        Ok(())
    }
}

async fn write_oversize_error<W: AsyncWriteExt + Unpin>(
    response_buffer: &mut Vec<u8>,
    writer: &mut W,
    max: usize,
) -> MCSResult<()> {
    let error = MCSError::InvalidParams(format!("Request exceeds maximum size of {max} bytes"));
    let response = JsonRpcResponse::error(None, error.error_code(), error.to_string());
    response_buffer.clear();
    serde_json::to_writer(&mut *response_buffer, &response)?;
    response_buffer.extend_from_slice(NEWLINE);
    writer.write_all(response_buffer).await?;
    writer.flush().await?;
    Ok(())
}

async fn process_one_line<W: AsyncWriteExt + Unpin>(
    line: &str,
    config: &Arc<Config>,
    response_buffer: &mut Vec<u8>,
    writer: &mut W,
) -> MCSResult<()> {
    let (response, notification) = match parse_request(line) {
        Ok(request) => {
            let notification = request.id.is_none();
            match tokio::time::timeout(
                config.server.request_timeout,
                process_request(&request, config),
            )
            .await
            {
                Ok(Ok(result)) => (
                    JsonRpcResponse::success(request.id, result),
                    notification,
                ),
                Ok(Err(error)) => (
                    JsonRpcResponse::error(request.id, error.error_code(), error.to_string()),
                    notification,
                ),
                Err(_) => {
                    let error = timeout_error(config);
                    (
                        JsonRpcResponse::error(request.id, error.error_code(), error.to_string()),
                        notification,
                    )
                }
            }
        }
        Err(error) => (parse_error(error), false),
    };

    if notification {
        return Ok(());
    }

    response_buffer.clear();
    serde_json::to_writer(&mut *response_buffer, &response)?;
    response_buffer.extend_from_slice(NEWLINE);
    writer.write_all(response_buffer).await?;
    writer.flush().await?;
    maybe_shrink_buffer(response_buffer);
    Ok(())
}

fn maybe_shrink_buffer(buffer: &mut Vec<u8>) {
    if buffer.capacity() > 1 << 20 {
        *buffer = Vec::with_capacity(65_536);
    }
}

pub async fn process_request(request: &JsonRpcRequest, config: &Config) -> MCSResult<Value> {
    match request.method.as_str() {
        "initialize" => handle_initialize(request),
        "tools/list" => handle_tools_list(config),
        "tools/call" => handle_tools_call(request, config).await,
        "prompts/list" => Ok(json!({ "prompts": [] })),
        "resources/list" => resources::list(request.params.as_ref(), config).await,
        "resources/read" => resources::read(request.params.as_ref(), config).await,
        "resources/templates/list" => Ok(resources::templates()),
        "ping" => Ok(Value::Null),
        method if method.starts_with("notifications/") => {
            tracing::trace!("Received notification: {method}");
            Ok(Value::Null)
        }
        _ => Err(MCSError::MethodNotFound(request.method.clone())),
    }
}

pub async fn process_request_http(request: &JsonRpcRequest, config: &Config) -> JsonRpcResponse {
    match tokio::time::timeout(
        config.server.request_timeout,
        process_request(request, config),
    )
    .await
    {
        Ok(Ok(result)) => JsonRpcResponse::success(request.id.clone(), result),
        Ok(Err(error)) => {
            JsonRpcResponse::error(request.id.clone(), error.error_code(), error.to_string())
        }
        Err(_) => {
            let error = timeout_error(config);
            JsonRpcResponse::error(request.id.clone(), error.error_code(), error.to_string())
        }
    }
}

fn timeout_error(config: &Config) -> MCSError {
    MCSError::FilesystemError(format!(
        "Request timed out after {}s",
        config.server.request_timeout.as_secs()
    ))
}

pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

pub fn is_supported_protocol_version(value: &str) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&value)
}

const SERVER_INSTRUCTIONS: &str = "Sandboxed filesystem MCP App Server. All file paths and file:// \
resources are restricted to the configured allowed directories. Use list_allowed_directories before \
browsing. Read-oriented tools can render the interactive filesystem browser. Tool results contain \
human-readable content and structuredContent; failures use isError=true so the model can correct \
arguments. Destructive, write, crypto, compression, and CSV tools remain opt-in by category.";

fn handle_initialize(request: &JsonRpcRequest) -> MCSResult<Value> {
    let protocol_version = request
        .params
        .as_ref()
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str)
        .filter(|version| is_supported_protocol_version(version))
        .unwrap_or(LATEST_PROTOCOL_VERSION);

    Ok(json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false }
        },
        "serverInfo": {
            "name": "mcp-filesystem",
            "title": "Filesystem MCP App Server",
            "version": env!("CARGO_PKG_VERSION"),
            "homepage": env!("CARGO_PKG_HOMEPAGE"),
            "repository": env!("CARGO_PKG_REPOSITORY"),
            "license": env!("CARGO_PKG_LICENSE")
        },
        "instructions": SERVER_INSTRUCTIONS
    }))
}

fn tool_success(tool_name: &str, value: Value) -> Value {
    let mut result = if value.get("content").is_some_and(Value::is_array) {
        let mut result = value;
        if result.get("isError").is_none() {
            result["isError"] = Value::Bool(false);
        }
        result
    } else {
        let text = serde_json::to_string(&value).unwrap_or_else(|_| "null".into());
        if value.is_object() {
            json!({
                "content": [{ "type": "text", "text": text }],
                "structuredContent": value,
                "isError": false
            })
        } else {
            json!({
                "content": [{ "type": "text", "text": text }],
                "isError": false
            })
        }
    };

    if has_browser_ui(tool_name) {
        let meta = result
            .as_object_mut()
            .expect("CallToolResult must be an object")
            .entry("_meta")
            .or_insert_with(|| json!({}));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert("ui".into(), json!({ "resourceUri": UI_RESOURCE_URI }));
            meta.insert(
                "openai/outputTemplate".into(),
                Value::String(UI_RESOURCE_URI.into()),
            );
        }
    }
    result
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true
    })
}

fn handle_tools_list(config: &Config) -> MCSResult<Value> {
    Ok(serde_json::from_slice(&config.tools_list_bytes)?)
}

async fn handle_tools_call(request: &JsonRpcRequest, config: &Config) -> MCSResult<Value> {
    let tool_name = request
        .params
        .as_ref()
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| MCSError::InvalidParams("Missing 'name' parameter".into()))?;
    let arguments = request
        .params
        .as_ref()
        .and_then(|params| params.get("arguments"));

    if !crate::tools::is_tool_available(tool_name, &config.server.enabled_categories) {
        return Err(method_not_found(tool_name));
    }

    if config.server.access_mode == crate::config::AccessMode::ReadOnly
        && crate::tools::is_write_tool(tool_name)
    {
        return Ok(tool_error(format!(
            "Operation '{tool_name}' is not allowed in read-only mode"
        )));
    }

    let result = match tool_name {
        "read_text_file" => actions::files::read_text_file(arguments, config).await,
        "read_media_file" => actions::files::read_media_file(arguments, config).await,
        "write_file" => actions::files::write_file(arguments, config).await,
        "edit_file" => actions::files::edit_file(arguments, config).await,
        "create_directory" => actions::files::create_directory(arguments, config).await,
        "list_directory" => actions::dirs::list_directory(arguments, config).await,
        "list_directory_with_sizes" => {
            actions::dirs::list_directory_with_sizes(arguments, config).await
        }
        "move_file" => actions::files::move_file(arguments, config).await,
        "copy_file" => actions::files::copy_file(arguments, config).await,
        "delete_file" => actions::files::delete_file(arguments, config).await,
        "delete_directory" => actions::files::delete_directory(arguments, config).await,
        "search_files" => actions::dirs::search_files(arguments, config).await,
        "directory_tree" => actions::dirs::directory_tree(arguments, config).await,
        "get_file_info" => actions::files::get_file_info(arguments, config).await,
        "list_allowed_directories" => {
            actions::files::list_allowed_directories(arguments, config).await
        }
        "hash_file" => actions::files::hash_file(arguments, config).await,
        "grep_files" => actions::dirs::grep_files(arguments, config).await,
        "set_permissions" => actions::files::set_permissions(arguments, config).await,
        "get_disk_usage" => actions::dirs::get_disk_usage(arguments, config).await,
        "create_symlink" => actions::files::create_symlink(arguments, config).await,
        "read_file_range" => actions::files::read_file_range(arguments, config).await,
        "compress_gzip" => actions::compress::compress_gzip(arguments, config).await,
        "decompress_gzip" => actions::compress::decompress_gzip(arguments, config).await,
        "compress_zstd" => actions::compress::compress_zstd(arguments, config).await,
        "decompress_zstd" => actions::compress::decompress_zstd(arguments, config).await,
        "compress_tar" => actions::compress::compress_tar(arguments, config).await,
        "decompress_tar" => actions::compress::decompress_tar(arguments, config).await,
        "encrypt_file" => actions::crypto::encrypt_file(arguments, config).await,
        "decrypt_file" => actions::crypto::decrypt_file(arguments, config).await,
        "generate_key" => actions::crypto::generate_key(arguments, config).await,
        "csv_create" => actions::csv::csv_create(arguments, config).await,
        "csv_read" => actions::csv::csv_read(arguments, config).await,
        "csv_add_row" => actions::csv::csv_add_row(arguments, config).await,
        "csv_update_cell" => actions::csv::csv_update_cell(arguments, config).await,
        "csv_remove_row" => actions::csv::csv_remove_row(arguments, config).await,
        "csv_add_column" => actions::csv::csv_add_column(arguments, config).await,
        "csv_remove_column" => actions::csv::csv_remove_column(arguments, config).await,
        "csv_rename_column" => actions::csv::csv_rename_column(arguments, config).await,
        "csv_read_column_values_range" => {
            actions::csv::csv_read_column_values_range(arguments, config).await
        }
        "csv_read_row_range" => actions::csv::csv_read_row_range(arguments, config).await,
        "csv_select_column_range" => {
            actions::csv::csv_select_column_range(arguments, config).await
        }
        unknown => Err(method_not_found(unknown)),
    };

    match result {
        Ok(value) => Ok(tool_success(tool_name, value)),
        Err(error) => {
            error!("Tool '{tool_name}' error: {error:?}");
            Ok(tool_error(error.to_string()))
        }
    }
}

fn method_not_found(name: &str) -> MCSError {
    MCSError::MethodNotFound(name.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison_accepts_bearer() {
        assert!(token_matches("secret", "secret"));
        assert!(token_matches("Bearer secret", "secret"));
        assert!(!token_matches("wrong", "secret"));
    }

    #[test]
    fn initialize_advertises_resources() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "initialize".into(),
            params: None,
            id: Some(json!(1)),
        };
        let result = handle_initialize(&request).unwrap();
        assert_eq!(result["protocolVersion"], LATEST_PROTOCOL_VERSION);
        assert!(result["capabilities"]["resources"].is_object());
    }

    #[test]
    fn browser_tools_reference_ui_resource() {
        let descriptor = enhance_tool_descriptor(&json!({
            "name": "list_directory",
            "description": "list",
            "inputSchema": { "type": "object" }
        }));
        assert_eq!(descriptor["_meta"]["ui"]["resourceUri"], UI_RESOURCE_URI);
        assert_eq!(
            descriptor["_meta"]["openai/outputTemplate"],
            UI_RESOURCE_URI
        );
    }

    #[test]
    fn tool_results_include_structured_content_and_ui_meta() {
        let result = tool_success("list_directory", json!({ "entries": [] }));
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["entries"], json!([]));
        assert_eq!(result["_meta"]["ui"]["resourceUri"], UI_RESOURCE_URI);
    }

    #[tokio::test]
    async fn capped_reader_rejects_oversize_lines() {
        let data = vec![b'a'; 1_024];
        let mut reader = tokio::io::BufReader::new(&data[..]);
        let mut buffer = Vec::new();
        let mut output = String::new();
        let result = read_line_capped(&mut reader, &mut buffer, &mut output, 100)
            .await
            .unwrap();
        assert!(matches!(result, LineRead::TooLong));
    }
}
