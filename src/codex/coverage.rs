//! Honest, schema-derived coverage for the Discord control plane.

use std::collections::BTreeSet;

use crate::codex::CapabilityCatalog;

pub const EXCLUDED_SERVER_REQUESTS: &[(&str, &str)] = &[
    (
        "account/chatgptAuthTokens/refresh",
        "requires local ChatGPT credentials; tokens must never transit Discord",
    ),
    (
        "attestation/generate",
        "requires local OS/hardware attestation material unavailable to the relay",
    ),
    (
        "item/tool/call",
        "dynamic client-side tool execution is not advertised by this host",
    ),
];

const INTERACTIVE_SERVER_REQUESTS: &[&str] = &[
    "applyPatchApproval",
    "execCommandApproval",
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/permissions/requestApproval",
    "item/tool/requestUserInput",
    "mcpServer/elicitation/request",
];
const HOST_SERVER_REQUESTS: &[&str] = &["currentTime/read"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientDimension {
    pub total: usize,
    pub dedicated_ux: usize,
    pub generic: usize,
    pub generic_methods: Vec<String>,
    pub exempt: usize,
    pub exempt_methods: Vec<String>,
    pub missing: Vec<String>,
    pub broken_forms: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerRequestDimension {
    pub total: usize,
    pub interactive: usize,
    pub host_handled: usize,
    pub negotiated_off: usize,
    pub unknown: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationDimension {
    pub total: usize,
    pub rendered: usize,
    pub reasoned_ignored: usize,
    pub unknown: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverageReport {
    pub client: ClientDimension,
    pub server_requests: ServerRequestDimension,
    pub notifications: NotificationDimension,
}

impl CoverageReport {
    #[must_use]
    pub fn compute(catalog: &CapabilityCatalog) -> Self {
        let mut client = ClientDimension {
            total: catalog.client_requests.len(),
            ..ClientDimension::default()
        };
        for method in &catalog.client_requests {
            if crate::discord::actions::dedicated_action(method).is_some() {
                match catalog.client_request(method) {
                    Some(capability) => {
                        if let Some(error) =
                            crate::discord::actions::dedicated_action_form_error(capability)
                        {
                            client.broken_forms.push(format!("{method}: {error}"));
                        } else {
                            client.dedicated_ux += 1;
                        }
                    }
                    None => client.missing.push(method.clone()),
                }
            } else if crate::discord::actions::action_exemption(method).is_some() {
                client.exempt += 1;
                client.exempt_methods.push(method.clone());
            } else if catalog.client_request(method).is_some() {
                client.generic += 1;
                client.generic_methods.push(method.clone());
            } else {
                client.missing.push(method.clone());
            }
        }

        let interactive: BTreeSet<_> = INTERACTIVE_SERVER_REQUESTS.iter().copied().collect();
        let host: BTreeSet<_> = HOST_SERVER_REQUESTS.iter().copied().collect();
        let excluded: BTreeSet<_> = EXCLUDED_SERVER_REQUESTS
            .iter()
            .map(|(method, _)| *method)
            .collect();
        let mut server_requests = ServerRequestDimension {
            total: catalog.server_requests.len(),
            ..ServerRequestDimension::default()
        };
        for method in &catalog.server_requests {
            if interactive.contains(method.as_str()) {
                server_requests.interactive += 1;
            } else if host.contains(method.as_str()) {
                server_requests.host_handled += 1;
            } else if excluded.contains(method.as_str()) {
                server_requests.negotiated_off += 1;
            } else {
                server_requests.unknown.push(method.clone());
            }
        }

        let known_notifications: BTreeSet<_> =
            crate::discord::notifications::CURRENT_NOTIFICATION_METHODS
                .iter()
                .copied()
                .collect();
        let mut notifications = NotificationDimension {
            total: catalog.server_notifications.len(),
            ..NotificationDimension::default()
        };
        for method in &catalog.server_notifications {
            if known_notifications.contains(method.as_str())
                && crate::discord::notifications::classify(method)
                    != crate::discord::notifications::NotificationDisposition::Unknown
            {
                if crate::discord::notifications::is_rendered(
                    crate::discord::notifications::classify(method),
                ) {
                    notifications.rendered += 1;
                } else {
                    notifications.reasoned_ignored += 1;
                }
            } else {
                notifications.unknown.push(method.clone());
            }
        }
        Self {
            client,
            server_requests,
            notifications,
        }
    }

    #[must_use]
    pub fn dedicated_percent(&self) -> f64 {
        let user_launchable = self.client.total.saturating_sub(self.client.exempt);
        if user_launchable == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            self.client.dedicated_ux as f64 / user_launchable as f64 * 100.0
        }
    }

    #[must_use]
    pub fn fully_accounted(&self) -> bool {
        self.client.dedicated_ux + self.client.generic + self.client.exempt == self.client.total
            && self.client.missing.is_empty()
            && self.client.broken_forms.is_empty()
            && self.server_requests.unknown.is_empty()
            && self.notifications.unknown.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::{CodexCommand, MethodCapability};
    use serde_json::json;

    #[test]
    fn generic_schema_action_does_not_count_as_dedicated_ux() {
        let mut catalog = CapabilityCatalog::default();
        for method in ["initialize", "thread/list", "future/value/write"] {
            catalog.client_requests.insert(method.to_owned());
            catalog.client_request_metadata.insert(
                method.to_owned(),
                MethodCapability {
                    method: method.to_owned(),
                    description: None,
                    title: None,
                    params_schema: Some(json!({"type":"object"})),
                },
            );
        }

        let report = CoverageReport::compute(&catalog);
        assert_eq!(report.client.dedicated_ux, 1);
        assert_eq!(report.client.generic, 1);
        assert_eq!(report.client.generic_methods, ["future/value/write"]);
        assert_eq!(report.client.exempt, 1);
        assert_eq!(report.dedicated_percent(), 50.0);
    }

    #[test]
    fn broken_dedicated_form_is_not_counted_as_coverage() {
        let mut catalog = CapabilityCatalog::default();
        let method = "thread/resume";
        catalog.client_requests.insert(method.to_owned());
        catalog.client_request_metadata.insert(
            method.to_owned(),
            MethodCapability {
                method: method.to_owned(),
                description: None,
                title: None,
                params_schema: Some(json!({
                    "type":"object",
                    "properties":{"items":{"type":"array"}}
                })),
            },
        );

        let report = CoverageReport::compute(&catalog);
        assert_eq!(report.client.dedicated_ux, 0);
        assert_eq!(report.client.broken_forms.len(), 1);
        assert!(!report.fully_accounted());
    }

    #[test]
    fn dedicated_registry_enforces_the_99_percent_floor() {
        let mut catalog = CapabilityCatalog::default();
        for spec in crate::discord::actions::DEDICATED_ACTIONS.iter().copied() {
            let method = spec.method;
            let params_schema =
                if spec.input == crate::discord::actions::InputStrategy::TaskTypedSchemaForm {
                    json!({
                        "type":"object",
                        "properties":{"threadId":{"type":"string"}},
                        "required":["threadId"]
                    })
                } else {
                    json!({"type":"object"})
                };
            catalog.client_requests.insert(method.to_owned());
            catalog.client_request_metadata.insert(
                method.to_owned(),
                MethodCapability {
                    method: method.to_owned(),
                    description: None,
                    title: None,
                    params_schema: Some(params_schema),
                },
            );
        }
        for method in crate::discord::actions::ACTION_EXEMPTIONS
            .iter()
            .map(|entry| entry.method)
        {
            catalog.client_requests.insert(method.to_owned());
            catalog.client_request_metadata.insert(
                method.to_owned(),
                MethodCapability {
                    method: method.to_owned(),
                    description: None,
                    title: None,
                    params_schema: Some(json!({"type":"object"})),
                },
            );
        }
        let report = CoverageReport::compute(&catalog);
        assert_eq!(report.client.total, 122);
        assert_eq!(report.client.dedicated_ux, 117);
        assert_eq!(report.client.exempt, 5);
        assert_eq!(report.client.generic, 0);
        assert!(report.client.generic_methods.is_empty());
        assert!(report.dedicated_percent() >= 99.0, "{report:#?}");
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server"]
    async fn live_schema_is_fully_accounted_without_raw_json_claims() {
        let output = tempfile::tempdir().unwrap();
        let catalog =
            CapabilityCatalog::generate_and_load(&CodexCommand::discover().unwrap(), output.path())
                .await
                .unwrap();
        let report = CoverageReport::compute(&catalog);
        // The installed bundle auto-updates, so exact totals drift (122 client
        // requests on codex-cli 0.144.1, 108 on 0.135.0-alpha.1). The durable
        // guarantee is full accounting: every discovered method, server
        // request, and notification must land in an explicit bucket, never in
        // missing/unknown.
        assert!(report.fully_accounted(), "{report:#?}");
        assert!(report.client.total >= 100, "{report:#?}");
        assert!(report.dedicated_percent() >= 99.0, "{report:#?}");
        assert_eq!(report.client.exempt, 5, "{report:#?}");
        assert!(report.client.generic_methods.is_empty(), "{report:#?}");
        assert!(report.server_requests.total >= 8, "{report:#?}");
        assert!(report.server_requests.interactive >= 7, "{report:#?}");
        assert!(report.notifications.total >= 60, "{report:#?}");
        println!(
            "installed coverage: {}/{} dedicated ({:.2}%), {} generic, {} notifications ({} rendered)",
            report.client.dedicated_ux,
            report.client.total - report.client.exempt,
            report.dedicated_percent(),
            report.client.generic,
            report.notifications.total,
            report.notifications.rendered,
        );
    }
}
