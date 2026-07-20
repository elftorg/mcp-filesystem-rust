use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
    pub id: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcRequest {
    pub fn from_value(value: &Value) -> std::result::Result<Self, crate::errors::MCSError> {
        let obj = value.as_object().ok_or_else(|| {
            crate::errors::MCSError::InvalidRequest("request must be a JSON object".into())
        })?;
        if obj.get("jsonrpc") != Some(&Value::String("2.0".into())) {
            return Err(crate::errors::MCSError::InvalidRequest(
                "jsonrpc must be \"2.0\"".into(),
            ));
        }
        let method = obj
            .get("method")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .ok_or_else(|| {
                crate::errors::MCSError::InvalidRequest("method must be a non-empty string".into())
            })?
            .to_string();
        let params = obj.get("params").cloned();
        let id = obj.get("id").cloned();
        if id
            .as_ref()
            .is_some_and(|v| !(v.is_string() || v.is_number() || v.is_null()))
        {
            return Err(crate::errors::MCSError::InvalidRequest(
                "id must be a string, number, or null".into(),
            ));
        }
        Ok(Self {
            jsonrpc: "2.0".into(),
            method,
            params,
            id,
        })
    }
}

impl JsonRpcResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            result: Some(result),
            error: None,
            id,
        }
    }

    pub fn error(id: Option<Value>, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
            id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_request_serde() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "read_text_file".to_string(),
            params: Some(json!({"path": "/tmp/test.txt"})),
            id: Some(Value::Number(1.into())),
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: JsonRpcRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.method, "read_text_file");
    }

    #[test]
    fn validates_jsonrpc_request() {
        assert!(
            JsonRpcRequest::from_value(&json!({"jsonrpc":"2.0","method":"ping","id":1})).is_ok()
        );
        assert!(JsonRpcRequest::from_value(&json!({"jsonrpc":"1.0","method":"ping"})).is_err());
        assert!(JsonRpcRequest::from_value(&json!([])).is_err());
    }

    #[test]
    fn test_response_success() {
        let resp =
            JsonRpcResponse::success(Some(Value::Number(1.into())), json!({"content": "hello"}));
        assert_eq!(resp.jsonrpc, "2.0");
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_response_error() {
        let resp = JsonRpcResponse::error(
            Some(Value::Number(1.into())),
            -32602,
            "Invalid params".into(),
        );
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32602);
    }
}
