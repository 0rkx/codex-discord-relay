//! Password-gated, fail-closed authorization for temporary unrestricted Codex turns.

use chrono::{DateTime, Utc};

mod audit;
mod authorization;
mod password;
mod rate_limit;
mod service;
mod sessions;

pub(crate) use audit::redact_secrets;
pub use audit::{AuditAction, AuditEvent, AuditSink, InMemoryAuditSink, redact_detail};
pub use authorization::{AccessDenied, SecurityContext, TrustBoundary};
pub use password::{
    Argon2idPasswordManager, MAX_PASSWORD_LENGTH, PasswordError, PasswordSubmission,
    StoredPasswordHash,
};
pub use rate_limit::{AttemptKey, AttemptLimiter, AttemptStatus};
pub use service::{GodModeError, GodModeService};
pub use sessions::{
    GodSession, SessionError, SessionIssue, SessionResolution, SessionScope, SessionStore,
};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::{DateTime, Duration, Utc};
    use secrecy::SecretString;

    use super::*;

    #[derive(Debug)]
    struct TestClock(Mutex<DateTime<Utc>>);

    impl TestClock {
        fn new(now: DateTime<Utc>) -> Self {
            Self(Mutex::new(now))
        }

        fn advance(&self, duration: Duration) {
            *self.0.lock().unwrap() += duration;
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }

    fn context() -> SecurityContext {
        SecurityContext {
            owner_id: 100,
            guild_id: Some(200),
            channel_id: 300,
            is_private_channel: true,
        }
    }

    fn submission(password: &str) -> PasswordSubmission {
        PasswordSubmission::try_after_delete(SecretString::from(password.to_owned()), true).unwrap()
    }

    fn service(clock: Arc<TestClock>, audit: InMemoryAuditSink) -> GodModeService {
        let passwords = Argon2idPasswordManager::default();
        let hash = passwords
            .hash(SecretString::from(
                "correct horse battery staple".to_owned(),
            ))
            .unwrap();
        GodModeService::new(
            hash,
            TrustBoundary::new(100, 200).unwrap(),
            Arc::new(audit),
            clock,
        )
    }

    struct FailingAuditSink;

    #[async_trait::async_trait]
    impl AuditSink for FailingAuditSink {
        async fn emit(&self, _event: AuditEvent) -> anyhow::Result<()> {
            anyhow::bail!("audit disk unavailable")
        }
    }

    #[tokio::test]
    async fn privileged_session_is_scoped_and_expires_after_ten_minutes() {
        let clock = Arc::new(TestClock::new(Utc::now()));
        let audit = InMemoryAuditSink::default();
        let service = service(Arc::clone(&clock), audit.clone());
        service
            .authenticate(
                &context(),
                submission("correct horse battery staple"),
                SessionScope::Task("task-1".to_owned()),
            )
            .await
            .unwrap();

        assert!(
            service
                .active_session(&context(), Some("task-1"))
                .await
                .is_some()
        );
        assert!(
            service
                .active_session(&context(), Some("task-2"))
                .await
                .is_none()
        );

        clock.advance(Duration::minutes(10));
        assert!(
            service
                .active_session(&context(), Some("task-1"))
                .await
                .is_none()
        );
        assert!(
            audit
                .events()
                .iter()
                .any(|event| event.action == AuditAction::SessionExpired)
        );
    }

    #[tokio::test]
    async fn fifth_failure_locks_authentication_and_never_audits_password() {
        let clock = Arc::new(TestClock::new(Utc::now()));
        let audit = InMemoryAuditSink::default();
        let service = service(clock, audit.clone());

        for attempt in 0..5 {
            let result = service
                .authenticate(
                    &context(),
                    submission("wrong password is long enough"),
                    SessionScope::Channel,
                )
                .await;
            if attempt < 4 {
                assert_eq!(result.unwrap_err(), GodModeError::InvalidPassword);
            } else {
                assert!(matches!(result, Err(GodModeError::Locked { .. })));
            }
        }
        let serialized = serde_json::to_string(&audit.events()).unwrap();
        assert!(!serialized.contains("wrong password"));
    }

    #[tokio::test]
    async fn revoke_immediately_restores_normal_permissions() {
        let clock = Arc::new(TestClock::new(Utc::now()));
        let service = service(clock, InMemoryAuditSink::default());
        let session = service
            .authenticate(
                &context(),
                submission("correct horse battery staple"),
                SessionScope::Channel,
            )
            .await
            .unwrap()
            .session;
        assert!(
            service
                .revoke(&context(), session.session_id, None)
                .await
                .unwrap()
        );
        assert!(service.active_session(&context(), None).await.is_none());
    }

    #[tokio::test]
    async fn new_session_revokes_the_previous_global_session() {
        let clock = Arc::new(TestClock::new(Utc::now()));
        let service = service(clock, InMemoryAuditSink::default());
        let first = service
            .authenticate(
                &context(),
                submission("correct horse battery staple"),
                SessionScope::Channel,
            )
            .await
            .unwrap()
            .session;
        let second_issue = service
            .authenticate(
                &context(),
                submission("correct horse battery staple"),
                SessionScope::Channel,
            )
            .await
            .unwrap();
        let second = second_issue.session;

        assert_eq!(
            second_issue
                .revoked
                .as_ref()
                .map(|session| session.session_id),
            Some(first.session_id)
        );

        assert!(
            service
                .active_session(&context(), None)
                .await
                .is_some_and(|active| active.session_id == second.session_id)
        );
    }

    #[tokio::test]
    async fn owner_can_emergency_revoke_from_another_private_channel() {
        let clock = Arc::new(TestClock::new(Utc::now()));
        let service = service(clock, InMemoryAuditSink::default());
        service
            .authenticate(
                &context(),
                submission("correct horse battery staple"),
                SessionScope::Channel,
            )
            .await
            .unwrap();
        let control_channel = SecurityContext {
            channel_id: 999,
            ..context()
        };
        assert!(service.revoke_active(&control_channel).await.unwrap());
        assert!(service.active_session(&context(), None).await.is_none());
    }

    #[tokio::test]
    async fn audit_failure_never_leaves_a_privileged_session_active() {
        let clock = Arc::new(TestClock::new(Utc::now()));
        let passwords = Argon2idPasswordManager::default();
        let hash = passwords
            .hash(SecretString::from(
                "correct horse battery staple".to_owned(),
            ))
            .unwrap();
        let service = GodModeService::new(
            hash,
            TrustBoundary::new(100, 200).unwrap(),
            Arc::new(FailingAuditSink),
            clock,
        );

        let error = service
            .authenticate(
                &context(),
                submission("correct horse battery staple"),
                SessionScope::Task("task-1".to_owned()),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, GodModeError::Audit(_)));
        assert!(service.active_global_session().await.is_none());
    }

    #[tokio::test]
    async fn pending_session_is_invisible_until_explicit_activation() {
        let clock = Arc::new(TestClock::new(Utc::now()));
        let service = service(clock, InMemoryAuditSink::default());
        let pending = service
            .authenticate_pending(
                &context(),
                submission("correct horse battery staple"),
                SessionScope::Task("task-1".to_owned()),
            )
            .await
            .unwrap();

        assert!(service.active_global_session().await.is_none());
        service.activate_pending(&context(), pending).await.unwrap();
        assert!(service.active_global_session().await.is_some());
    }

    #[tokio::test]
    async fn ordinary_expiry_read_cannot_consume_monitor_cleanup_payload() {
        let clock = Arc::new(TestClock::new(Utc::now()));
        let service = service(Arc::clone(&clock), InMemoryAuditSink::default());
        service
            .authenticate(
                &context(),
                submission("correct horse battery staple"),
                SessionScope::Task("task-1".to_owned()),
            )
            .await
            .unwrap();

        clock.advance(Duration::minutes(10));
        assert!(
            service
                .active_session(&context(), Some("task-1"))
                .await
                .is_none()
        );
        assert!(service.cleanup_required_for("task-1"));
        assert_eq!(
            service
                .expire_global_session()
                .await
                .and_then(|session| match session.scope {
                    SessionScope::Task(task_id) => Some(task_id),
                    SessionScope::Channel => None,
                })
                .as_deref(),
            Some("task-1")
        );
    }

    #[tokio::test]
    async fn revoke_marks_task_cleaning_before_async_cleanup() {
        let clock = Arc::new(TestClock::new(Utc::now()));
        let service = service(clock, InMemoryAuditSink::default());
        service
            .authenticate(
                &context(),
                submission("correct horse battery staple"),
                SessionScope::Task("task-1".to_owned()),
            )
            .await
            .unwrap();

        assert!(service.revoke_active(&context()).await.unwrap());
        assert!(service.cleanup_required_for("task-1"));
        service.cleanup_finished("task-1");
        assert!(!service.cleanup_required_for("task-1"));
    }

    #[test]
    fn ttl_cannot_exceed_ten_minutes() {
        let clock: Arc<dyn Clock> = Arc::new(TestClock::new(Utc::now()));
        assert!(matches!(
            SessionStore::with_ttl(clock, Duration::minutes(11)),
            Err(SessionError::InvalidTtl)
        ));
    }
}
