#![allow(clippy::missing_errors_doc)]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Stdio,
};

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tokio::fs;

use super::client::CodexCommand;

#[derive(Debug, Error)]
pub enum CapabilityError {
    #[error("failed to access schema path: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid Codex JSON schema: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Codex schema generation failed ({status}): {stderr}")]
    Generation { status: i32, stderr: String },
    #[error("Codex schema generation did not finish within {0} seconds")]
    Timeout(u64),
}

/// Startup must never hang on a wedged `generate-json-schema` child; callers
/// degrade to an empty catalog instead.
const GENERATION_TIMEOUT_SECONDS: u64 = 120;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MethodCapability {
    /// The JSON-RPC method name (for example `thread/start`).
    pub method: String,
    /// Human-readable description emitted by Codex's generated schema.
    pub description: Option<String>,
    /// Schema title, when the protocol generator emits one.
    pub title: Option<String>,
    /// The complete parameter schema. Local `#/definitions/...` references are
    /// expanded so callers can render forms without needing the source file.
    pub params_schema: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapabilityCatalog {
    pub client_requests: BTreeSet<String>,
    pub server_requests: BTreeSet<String>,
    pub server_notifications: BTreeSet<String>,
    /// Metadata is retained separately from the compatibility sets above so
    /// existing callers can keep doing cheap membership checks. Every method
    /// in those sets has a corresponding metadata entry when its schema has a
    /// `params` property.
    pub client_request_metadata: BTreeMap<String, MethodCapability>,
    pub server_request_metadata: BTreeMap<String, MethodCapability>,
    pub server_notification_metadata: BTreeMap<String, MethodCapability>,
}

impl CapabilityCatalog {
    pub async fn load(schema_directory: impl AsRef<Path>) -> Result<Self, CapabilityError> {
        let root = schema_directory.as_ref();
        let client_request_metadata = metadata_from_file(&root.join("ClientRequest.json")).await?;
        let server_request_metadata = metadata_from_file(&root.join("ServerRequest.json")).await?;
        let server_notification_metadata =
            metadata_from_file(&root.join("ServerNotification.json")).await?;
        Ok(Self {
            client_requests: client_request_metadata.keys().cloned().collect(),
            server_requests: server_request_metadata.keys().cloned().collect(),
            server_notifications: server_notification_metadata.keys().cloned().collect(),
            client_request_metadata,
            server_request_metadata,
            server_notification_metadata,
        })
    }

    pub async fn generate_and_load(
        command: &CodexCommand,
        output_directory: impl AsRef<Path>,
    ) -> Result<Self, CapabilityError> {
        let output_directory = output_directory.as_ref();
        fs::create_dir_all(output_directory).await?;
        let child = command
            .process()
            .arg("app-server")
            .arg("generate-json-schema")
            .arg("--experimental")
            .arg("--out")
            .arg(output_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(GENERATION_TIMEOUT_SECONDS),
            child.wait_with_output(),
        )
        .await
        {
            Ok(output) => output?,
            Err(_) => return Err(CapabilityError::Timeout(GENERATION_TIMEOUT_SECONDS)),
        };
        if !output.status.success() {
            return Err(CapabilityError::Generation {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(4_000)
                    .collect(),
            });
        }
        Self::load(output_directory).await
    }

    #[must_use]
    pub fn supports_client_request(&self, method: &str) -> bool {
        self.client_requests.contains(method)
    }

    /// Return schema metadata for a client request, if the generated schema
    /// contains a method branch for it.
    #[must_use]
    pub fn client_request(&self, method: &str) -> Option<&MethodCapability> {
        self.client_request_metadata.get(method)
    }

    /// Return schema metadata for a server-initiated request.
    #[must_use]
    pub fn server_request(&self, method: &str) -> Option<&MethodCapability> {
        self.server_request_metadata.get(method)
    }

    /// Return schema metadata for a server notification.
    #[must_use]
    pub fn server_notification(&self, method: &str) -> Option<&MethodCapability> {
        self.server_notification_metadata.get(method)
    }
}

async fn metadata_from_file(
    path: &Path,
) -> Result<BTreeMap<String, MethodCapability>, CapabilityError> {
    let value: Value = serde_json::from_slice(&fs::read(path).await?)?;
    let mut methods = BTreeMap::new();
    collect_method_metadata(&value, &value, &mut methods);
    Ok(methods)
}

#[cfg(test)]
fn collect_method_enums(value: &Value, methods: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            if let Some(values) = object
                .get("properties")
                .and_then(|properties| properties.get("method"))
                .and_then(|method| method.get("enum"))
                .and_then(Value::as_array)
            {
                methods.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            for child in object.values() {
                collect_method_enums(child, methods);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_method_enums(child, methods);
            }
        }
        _ => {}
    }
}

fn collect_method_metadata(
    value: &Value,
    root: &Value,
    methods: &mut BTreeMap<String, MethodCapability>,
) {
    let Value::Object(object) = value else {
        if let Value::Array(values) = value {
            for child in values {
                collect_method_metadata(child, root, methods);
            }
        }
        return;
    };

    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        let method_values = properties
            .get("method")
            .and_then(Value::as_object)
            .and_then(|method| method.get("enum"))
            .and_then(Value::as_array);
        if let Some(method_values) = method_values {
            let params_schema = properties
                .get("params")
                .map(|schema| resolve_local_refs(schema, root, 0));
            let description = object
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let title = object
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned);
            for method in method_values.iter().filter_map(Value::as_str) {
                let metadata = MethodCapability {
                    method: method.to_owned(),
                    description: description.clone(),
                    title: title.clone(),
                    params_schema: params_schema.clone(),
                };
                // A few schema versions include an alias branch for the same
                // method. Prefer the branch with actual parameter metadata.
                methods
                    .entry(method.to_owned())
                    .and_modify(|existing| {
                        if existing.params_schema.is_none() && metadata.params_schema.is_some() {
                            *existing = metadata.clone();
                        } else if existing.description.is_none() && metadata.description.is_some() {
                            existing.description.clone_from(&metadata.description);
                        }
                    })
                    .or_insert(metadata);
            }
        }
    }

    for child in object.values() {
        collect_method_metadata(child, root, methods);
    }
}

/// Expand local JSON-schema references while preserving every other schema
/// keyword. A depth cap protects against recursive definitions in future
/// protocol versions.
fn resolve_local_refs(value: &Value, root: &Value, depth: usize) -> Value {
    if depth >= 32 {
        return value.clone();
    }
    if let Some(reference) = value.get("$ref").and_then(Value::as_str)
        && let Some(pointer) = reference.strip_prefix("#")
        && let Some(target) = root.pointer(pointer)
    {
        return resolve_local_refs(target, root, depth + 1);
    }
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, child)| (key.clone(), resolve_local_refs(child, root, depth + 1)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|child| resolve_local_refs(child, root, depth + 1))
                .collect(),
        ),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::CodexCommand;
    use serde_json::json;

    #[test]
    fn extracts_singleton_and_multi_value_method_enums() {
        let schema = json!({
            "definitions": {
                "One": {"properties": {"method": {"enum": ["thread/list"]}}},
                "Two": {"properties": {"method": {"enum": ["turn/start", "turn/steer"]}}},
                "Noise": {"properties": {"status": {"enum": ["ok"]}}}
            }
        });
        let mut methods = BTreeSet::new();
        collect_method_enums(&schema, &mut methods);
        assert_eq!(
            methods,
            BTreeSet::from([
                "thread/list".to_owned(),
                "turn/start".to_owned(),
                "turn/steer".to_owned()
            ])
        );
    }

    #[test]
    fn extracts_descriptions_and_expands_parameter_references() {
        let schema = json!({
            "definitions": {
                "Params": {
                    "type": "object",
                    "properties": {"cwd": {"type": "string"}}
                }
            },
            "oneOf": [{
                "title": "Thread/startRequest",
                "description": "Start a Codex thread.",
                "properties": {
                    "method": {"enum": ["thread/start"]},
                    "params": {"$ref": "#/definitions/Params"}
                }
            }]
        });
        let mut methods = BTreeMap::new();
        collect_method_metadata(&schema, &schema, &mut methods);
        let metadata = methods.get("thread/start").unwrap();
        assert_eq!(
            metadata.description.as_deref(),
            Some("Start a Codex thread.")
        );
        assert_eq!(metadata.title.as_deref(), Some("Thread/startRequest"));
        assert_eq!(metadata.params_schema.as_ref().unwrap()["type"], "object");
        assert_eq!(
            metadata.params_schema.as_ref().unwrap()["properties"]["cwd"]["type"],
            "string"
        );
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server"]
    async fn live_schema_generation_discovers_core_methods() {
        let output = tempfile::tempdir().unwrap();
        let catalog =
            CapabilityCatalog::generate_and_load(&CodexCommand::discover().unwrap(), output.path())
                .await
                .unwrap();
        assert!(catalog.supports_client_request("thread/list"));
        assert!(catalog.supports_client_request("turn/start"));
        assert!(
            catalog
                .server_requests
                .contains("item/commandExecution/requestApproval")
        );
        assert!(catalog.server_notifications.contains("turn/completed"));
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server"]
    async fn live_schema_has_complete_client_request_coverage() {
        let output = tempfile::tempdir().unwrap();
        let catalog =
            CapabilityCatalog::generate_and_load(&CodexCommand::discover().unwrap(), output.path())
                .await
                .unwrap();

        // The installed bundle auto-updates (122 client requests on codex-cli
        // 0.144.1, 108 on 0.135.0-alpha.1), so pin invariants rather than
        // exact totals: sane floors, and complete metadata so every discovered
        // method can render a schema-generated form.
        assert!(catalog.client_requests.len() >= 100);
        assert!(catalog.server_requests.len() >= 8);
        assert!(catalog.server_notifications.len() >= 60);
        assert_eq!(
            catalog.client_request_metadata.len(),
            catalog.client_requests.len()
        );
        assert_eq!(
            catalog.server_request_metadata.len(),
            catalog.server_requests.len()
        );
        assert_eq!(
            catalog.server_notification_metadata.len(),
            catalog.server_notifications.len()
        );
        assert!(
            catalog
                .client_request_metadata
                .values()
                .all(|method| method.params_schema.is_some())
        );
    }
}
