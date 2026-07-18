#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationDisposition {
    AgentMessageDelta,
    HighVolumeStream,
    TaskLifecycle,
    PlanUpdated,
    ItemActivity,
    TokenUsage,
    RequestResolved,
    RunnerStatus,
    UserAlert,
    AuditOnly,
    Unknown,
}

pub const CURRENT_NOTIFICATION_METHODS: &[&str] = &[
    "account/login/completed",
    "account/rateLimits/updated",
    "account/updated",
    "app/list/updated",
    "command/exec/outputDelta",
    "configWarning",
    "deprecationNotice",
    "error",
    "externalAgentConfig/import/completed",
    "externalAgentConfig/import/progress",
    "fs/changed",
    "fuzzyFileSearch/sessionCompleted",
    "fuzzyFileSearch/sessionUpdated",
    "guardianWarning",
    "hook/completed",
    "hook/started",
    "item/agentMessage/delta",
    "item/autoApprovalReview/completed",
    "item/autoApprovalReview/started",
    "item/commandExecution/outputDelta",
    "item/commandExecution/terminalInteraction",
    "item/completed",
    "item/fileChange/outputDelta",
    "item/fileChange/patchUpdated",
    "item/mcpToolCall/progress",
    "item/plan/delta",
    "item/reasoning/summaryPartAdded",
    "item/reasoning/summaryTextDelta",
    "item/reasoning/textDelta",
    "item/started",
    "mcpServer/oauthLogin/completed",
    "mcpServer/startupStatus/updated",
    "model/rerouted",
    "model/safetyBuffering/updated",
    "model/verification",
    "process/exited",
    "process/outputDelta",
    "remoteControl/status/changed",
    "serverRequest/resolved",
    "skills/changed",
    "thread/archived",
    "thread/closed",
    "thread/compacted",
    "thread/deleted",
    "thread/environment/connected",
    "thread/environment/disconnected",
    "thread/goal/cleared",
    "thread/goal/updated",
    "thread/name/updated",
    "thread/realtime/closed",
    "thread/realtime/error",
    "thread/realtime/itemAdded",
    "thread/realtime/outputAudio/delta",
    "thread/realtime/sdp",
    "thread/realtime/started",
    "thread/realtime/transcript/delta",
    "thread/realtime/transcript/done",
    "thread/settings/updated",
    "thread/started",
    "thread/status/changed",
    "thread/tokenUsage/updated",
    "thread/unarchived",
    "turn/completed",
    "turn/diff/updated",
    "turn/moderationMetadata",
    "turn/plan/updated",
    "turn/started",
    "warning",
    "windows/worldWritableWarning",
    "windowsSandbox/setupCompleted",
];

#[must_use]
pub fn classify(method: &str) -> NotificationDisposition {
    match method {
        "item/agentMessage/delta" => NotificationDisposition::AgentMessageDelta,
        "turn/plan/updated" => NotificationDisposition::PlanUpdated,
        "item/started" | "item/completed" => NotificationDisposition::ItemActivity,
        "thread/tokenUsage/updated" => NotificationDisposition::TokenUsage,
        "serverRequest/resolved" => NotificationDisposition::RequestResolved,
        "command/exec/outputDelta"
        | "externalAgentConfig/import/progress"
        | "fuzzyFileSearch/sessionUpdated"
        | "item/commandExecution/outputDelta"
        | "item/fileChange/outputDelta"
        | "item/fileChange/patchUpdated"
        | "item/mcpToolCall/progress"
        | "item/plan/delta"
        | "item/reasoning/summaryPartAdded"
        | "item/reasoning/summaryTextDelta"
        | "item/reasoning/textDelta"
        | "process/outputDelta"
        | "thread/realtime/outputAudio/delta"
        | "thread/realtime/transcript/delta" => NotificationDisposition::HighVolumeStream,
        "thread/archived"
        | "thread/closed"
        | "thread/compacted"
        | "thread/deleted"
        | "thread/name/updated"
        | "thread/unarchived"
        | "turn/completed"
        | "turn/started" => NotificationDisposition::TaskLifecycle,
        "account/login/completed"
        | "account/rateLimits/updated"
        | "account/updated"
        | "mcpServer/oauthLogin/completed"
        | "remoteControl/status/changed"
        | "thread/environment/connected"
        | "thread/environment/disconnected"
        | "windowsSandbox/setupCompleted" => NotificationDisposition::RunnerStatus,
        "configWarning"
        | "deprecationNotice"
        | "error"
        | "guardianWarning"
        | "model/rerouted"
        | "thread/realtime/error"
        | "warning"
        | "windows/worldWritableWarning" => NotificationDisposition::UserAlert,
        "app/list/updated"
        | "externalAgentConfig/import/completed"
        | "fs/changed"
        | "fuzzyFileSearch/sessionCompleted"
        | "hook/completed"
        | "hook/started"
        | "item/autoApprovalReview/completed"
        | "item/autoApprovalReview/started"
        | "item/commandExecution/terminalInteraction"
        | "mcpServer/startupStatus/updated"
        | "model/safetyBuffering/updated"
        | "model/verification"
        | "process/exited"
        | "skills/changed"
        | "thread/goal/cleared"
        | "thread/goal/updated"
        | "thread/realtime/closed"
        | "thread/realtime/itemAdded"
        | "thread/realtime/sdp"
        | "thread/realtime/started"
        | "thread/realtime/transcript/done"
        | "thread/settings/updated"
        | "thread/started"
        | "thread/status/changed"
        | "turn/diff/updated"
        | "turn/moderationMetadata" => NotificationDisposition::AuditOnly,
        _ => NotificationDisposition::Unknown,
    }
}

#[must_use]
pub const fn is_rendered(disposition: NotificationDisposition) -> bool {
    !matches!(
        disposition,
        NotificationDisposition::HighVolumeStream
            | NotificationDisposition::AuditOnly
            | NotificationDisposition::Unknown
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_installed_notification_has_an_explicit_disposition() {
        assert_eq!(CURRENT_NOTIFICATION_METHODS.len(), 70);
        for method in CURRENT_NOTIFICATION_METHODS {
            assert_ne!(
                classify(method),
                NotificationDisposition::Unknown,
                "{method} lacks a disposition"
            );
        }
    }

    #[test]
    fn only_agent_message_delta_is_rendered_as_the_answer() {
        assert_eq!(
            classify("item/agentMessage/delta"),
            NotificationDisposition::AgentMessageDelta
        );
        assert_eq!(
            classify("process/outputDelta"),
            NotificationDisposition::HighVolumeStream
        );
    }

    #[test]
    fn rendered_and_reasoned_ignore_counts_are_explicit() {
        let rendered = CURRENT_NOTIFICATION_METHODS
            .iter()
            .filter(|method| is_rendered(classify(method)))
            .count();
        assert_eq!(rendered, 30);
        assert_eq!(CURRENT_NOTIFICATION_METHODS.len() - rendered, 40);
    }

    #[test]
    fn thread_closed_updates_the_task_lifecycle() {
        assert_eq!(
            classify("thread/closed"),
            NotificationDisposition::TaskLifecycle
        );
    }

    #[tokio::test]
    #[ignore = "requires the locally installed Codex app-server"]
    async fn live_schema_notifications_match_the_explicit_router() {
        let output = tempfile::tempdir().unwrap();
        let catalog = crate::codex::CapabilityCatalog::generate_and_load(
            &crate::codex::CodexCommand::discover().unwrap(),
            output.path(),
        )
        .await
        .unwrap();
        // The installed bundle auto-updates, so the router is allowed to know
        // about more methods than the running Codex exposes. What must never
        // happen is the reverse: an installed notification the router cannot
        // classify would silently fall into Unknown at runtime.
        let unrouted: Vec<_> = catalog
            .server_notifications
            .iter()
            .filter(|method| classify(method) == NotificationDisposition::Unknown)
            .collect();
        assert!(
            unrouted.is_empty(),
            "installed notifications missing a disposition: {unrouted:?}"
        );
        let dormant: Vec<_> = CURRENT_NOTIFICATION_METHODS
            .iter()
            .filter(|method| !catalog.server_notifications.contains(**method))
            .collect();
        println!(
            "installed notifications: {}; router extras not in this Codex build: {dormant:?}",
            catalog.server_notifications.len()
        );
    }
}
