//! Shared JSON-RPC argument extraction helpers used by every action module.
//!
//! Tool arguments arrive as an optional `serde_json::Value` object; these
//! helpers pull typed fields out of it, returning `InvalidParams` for missing
//! required fields and `None`/defaults for optional ones.

use serde_json::Value;

use crate::errors::{MCSError, Result};

/// Required string argument.
pub(crate) fn get_str_arg(args: Option<&Value>, name: &str) -> Result<String> {
    args.and_then(|a| a.get(name))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| MCSError::InvalidParams(format!("Missing required parameter: '{name}'")))
}

/// Optional string argument.
pub(crate) fn get_opt_str(args: Option<&Value>, name: &str) -> Option<String> {
    args.and_then(|a| a.get(name))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Required signed integer argument.
pub(crate) fn get_i64_arg(args: Option<&Value>, name: &str) -> Result<i64> {
    args.and_then(|a| a.get(name))
        .and_then(|v| v.as_i64())
        .ok_or_else(|| MCSError::InvalidParams(format!("Missing required parameter: '{name}'")))
}

/// Optional signed integer argument.
pub(crate) fn get_opt_i64(args: Option<&Value>, name: &str) -> Option<i64> {
    args.and_then(|a| a.get(name)).and_then(|v| v.as_i64())
}

/// Optional unsigned integer argument.
pub(crate) fn get_opt_u64(args: Option<&Value>, name: &str) -> Option<u64> {
    args.and_then(|a| a.get(name)).and_then(|v| v.as_u64())
}

/// Optional boolean argument.
pub(crate) fn get_opt_bool(args: Option<&Value>, name: &str) -> Option<bool> {
    args.and_then(|a| a.get(name)).and_then(|v| v.as_bool())
}

/// Optional array-of-strings argument. Returns `None` when the field is absent
/// or not an array; non-string elements are skipped. Callers that want a plain
/// `Vec` use `.unwrap_or_default()`.
pub(crate) fn get_opt_str_array(args: Option<&Value>, name: &str) -> Option<Vec<String>> {
    let arr = args.and_then(|a| a.get(name)).and_then(|v| v.as_array())?;
    Some(
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
    )
}

/// Required array-of-strings argument. Every element must be a string.
pub(crate) fn get_str_array(args: Option<&Value>, name: &str) -> Result<Vec<String>> {
    let arr = args
        .and_then(|a| a.get(name))
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            MCSError::InvalidParams(format!("Missing required: '{name}' (must be an array)"))
        })?;
    let mut result = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        let s = v
            .as_str()
            .ok_or_else(|| MCSError::InvalidParams(format!("'{name}[{i}]' must be a string")))?;
        result.push(s.to_string());
    }
    Ok(result)
}

/// Required `edits` array (each element an object with `oldText`/`newText`).
pub(crate) fn get_edits_arg(args: Option<&Value>) -> Result<Vec<Value>> {
    args.and_then(|a| a.get("edits"))
        .and_then(|v| v.as_array())
        .cloned()
        .ok_or_else(|| {
            MCSError::InvalidParams("Missing required parameter: 'edits' (array)".into())
        })
}

/// Optional array-of-arrays-of-strings argument (CSV row batches). Returns
/// `None` if the field is absent or any element is not an array.
pub(crate) fn get_opt_str_array_of_arrays(
    args: Option<&Value>,
    name: &str,
) -> Option<Vec<Vec<String>>> {
    let arr = args.and_then(|a| a.get(name)).and_then(|v| v.as_array())?;
    let mut result = Vec::new();
    for item in arr {
        let inner = item.as_array()?;
        result.push(inner.iter().map(val_as_string).collect());
    }
    Some(result)
}

/// Coerce a JSON value to a CSV cell string: strings pass through unquoted,
/// null becomes empty, everything else uses its JSON representation.
pub(crate) fn val_as_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
