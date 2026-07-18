use std::{
    collections::HashSet,
    sync::{Arc, PoisonError, RwLock},
};

use chrono::Duration;
use serde_json::json;
use uuid::Uuid;

use super::{
    AccessDenied, Argon2idPasswordManager, AttemptKey, AttemptLimiter, AttemptStatus, AuditAction,
    AuditEvent, AuditSink, Clock, GodSession, PasswordSubmission, SecurityContext, SessionError,
    SessionIssue, SessionResolution, SessionScope, SessionStore, StoredPasswordHash, TrustBoundary,
};

pub struct GodModeService {
    password_hash: StoredPasswordHash,
    passwords: Argon2idPasswordManager,
    boundary: TrustBoundary,
    limiter: AttemptLimiter,
    sessions: SessionStore,
    audit: Arc<dyn AuditSink>,
    clock: Arc<dyn Clock>,
    cleanup_required: RwLock<HashSet<String>>,
}

impl GodModeService {
    pub fn new(
        password_hash: StoredPasswordHash,
        boundary: TrustBoundary,
        audit: Arc<dyn AuditSink>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            password_hash,
            passwords: Argon2idPasswordManager::default(),
            boundary,
            limiter: AttemptLimiter::new(Arc::clone(&clock)),
            sessions: SessionStore::new(Arc::clone(&clock)),
            audit,
            clock,
            cleanup_required: RwLock::new(HashSet::new()),
        }
    }

    /// # Errors
    ///
    /// Returns an error unless the requested TTL is positive and at most ten minutes.
    pub fn with_ttl(
        password_hash: StoredPasswordHash,
        boundary: TrustBoundary,
        audit: Arc<dyn AuditSink>,
        clock: Arc<dyn Clock>,
        ttl: Duration,
    ) -> Result<Self, GodModeError> {
        Ok(Self {
            password_hash,
            passwords: Argon2idPasswordManager::default(),
            boundary,
            limiter: AttemptLimiter::new(Arc::clone(&clock)),
            sessions: SessionStore::with_ttl(Arc::clone(&clock), ttl)?,
            audit,
            clock,
            cleanup_required: RwLock::new(HashSet::new()),
        })
    }

    #[cfg(test)]
    pub(crate) async fn authenticate(
        &self,
        context: &SecurityContext,
        submission: PasswordSubmission,
        scope: SessionScope,
    ) -> Result<SessionIssue, GodModeError> {
        let pending = self
            .authenticate_pending(context, submission, scope)
            .await?;
        self.activate_pending(context, pending).await
    }

    /// Verify a password and construct a grant without exposing it to request
    /// handlers. The Discord host activates it only after the red warning is
    /// visible and any replaced task has been safely cleaned.
    pub async fn authenticate_pending(
        &self,
        context: &SecurityContext,
        submission: PasswordSubmission,
        scope: SessionScope,
    ) -> Result<GodSession, GodModeError> {
        if let Err(reason) = self.boundary.authorize(context) {
            self.emit(
                AuditAction::AccessDenied,
                context,
                None,
                None,
                json!({
                    "reason": reason.to_string()
                }),
            )
            .await?;
            return Err(reason.into());
        }

        let key = attempt_key(context)?;
        if let AttemptStatus::Locked {
            retry_after_seconds,
        } = self.limiter.status(key)
        {
            self.emit(
                AuditAction::AuthenticationLocked,
                context,
                None,
                scope.task_id(),
                json!({"retry_after_seconds": retry_after_seconds}),
            )
            .await?;
            return Err(GodModeError::Locked {
                retry_after_seconds,
            });
        }

        if !self.passwords.verify(&self.password_hash, &submission) {
            let status = self.limiter.record_failure(key);
            self.emit(
                AuditAction::AuthenticationFailed,
                context,
                None,
                scope.task_id(),
                json!({}),
            )
            .await?;
            return match status {
                AttemptStatus::Allowed => Err(GodModeError::InvalidPassword),
                AttemptStatus::Locked {
                    retry_after_seconds,
                } => Err(GodModeError::Locked {
                    retry_after_seconds,
                }),
            };
        }

        self.limiter.reset(key);
        Ok(self.sessions.prepare(context, scope)?)
    }

    /// Make a verified pending grant active and durably audit the transition.
    pub async fn activate_pending(
        &self,
        context: &SecurityContext,
        pending: GodSession,
    ) -> Result<SessionIssue, GodModeError> {
        self.boundary.authorize(context)?;
        if pending.owner_id != context.owner_id
            || Some(pending.guild_id) != context.guild_id
            || pending.channel_id != context.channel_id
        {
            return Err(AccessDenied::WrongOwner.into());
        }
        if self.sessions.is_expired(&pending) {
            return Err(GodModeError::PendingExpired);
        }
        let issued = self.sessions.activate(pending);
        let audit_result = async {
            if let Some(revoked) = issued.revoked.as_ref() {
                let revoked_context = context_from_session(revoked);
                self.emit(
                    AuditAction::SessionRevoked,
                    &revoked_context,
                    Some(revoked),
                    revoked.scope.task_id(),
                    json!({"reason": "replaced_by_new_session"}),
                )
                .await?;
            }
            self.emit(
                AuditAction::SessionStarted,
                context,
                Some(&issued.session),
                issued.session.scope.task_id(),
                json!({
                    "scope": issued.session.scope,
                    "expires_at": issued.session.expires_at,
                    "warning": "unrestricted local execution enabled"
                }),
            )
            .await
        }
        .await;
        if let Err(error) = audit_result {
            // Never expose a privileged grant that could not be durably audited.
            self.sessions.revoke(issued.session.session_id);
            if let Some(previous) = issued.revoked.clone() {
                self.sessions.restore(previous);
            }
            return Err(error);
        }
        if let Some(previous) = issued.revoked.as_ref() {
            self.mark_cleanup_required(previous);
        }
        Ok(issued)
    }

    /// Return the global grant only when it is active for this exact context.
    pub async fn active_session(
        &self,
        context: &SecurityContext,
        task_id: Option<&str>,
    ) -> Option<GodSession> {
        if self.boundary.authorize(context).is_err() {
            return None;
        }
        match self.sessions.active_for(context, task_id) {
            SessionResolution::Active(session) => Some(session),
            SessionResolution::Expired(session) => {
                self.mark_cleanup_required(&session);
                let _ = self
                    .emit(
                        AuditAction::SessionExpired,
                        context,
                        Some(&session),
                        task_id,
                        json!({}),
                    )
                    .await;
                None
            }
            SessionResolution::Missing | SessionResolution::ScopeMismatch => None,
        }
    }

    /// Return the sole active grant for owner dashboards without weakening its
    /// task/channel enforcement when a turn actually starts.
    pub async fn expire_global_session(&self) -> Option<GodSession> {
        let expired = self.sessions.purge_expired().into_iter().next();
        if let Some(session) = expired.as_ref() {
            self.mark_cleanup_required(session);
            let context = context_from_session(session);
            let _ = self
                .emit(
                    AuditAction::SessionExpired,
                    &context,
                    Some(session),
                    session.scope.task_id(),
                    json!({}),
                )
                .await;
        }
        expired
    }

    /// Return the sole active grant for owner dashboards without weakening its
    /// task/channel enforcement when a turn actually starts.
    pub async fn active_global_session(&self) -> Option<GodSession> {
        let session = self.sessions.snapshot()?;
        if self.sessions.is_expired(&session) {
            self.mark_cleanup_required(&session);
            None
        } else {
            Some(session)
        }
    }

    /// Bot-friendly emergency revocation from any authorized private channel.
    ///
    /// # Errors
    ///
    /// Returns an access error unless the caller is the configured owner in the configured guild.
    pub async fn revoke_active(&self, context: &SecurityContext) -> Result<bool, GodModeError> {
        self.boundary.authorize(context)?;
        let Some(session) = self.sessions.revoke_active() else {
            return Ok(false);
        };
        self.mark_cleanup_required(&session);
        let revoked_context = context_from_session(&session);
        self.emit(
            AuditAction::SessionRevoked,
            &revoked_context,
            Some(&session),
            session.scope.task_id(),
            json!({"reason": "owner_emergency_revoke"}),
        )
        .await?;
        Ok(true)
    }

    /// # Errors
    ///
    /// Returns an access error unless the caller is inside the configured trust boundary.
    pub async fn revoke(
        &self,
        context: &SecurityContext,
        session_id: Uuid,
        task_id: Option<&str>,
    ) -> Result<bool, GodModeError> {
        self.boundary.authorize(context)?;
        match self.sessions.resolve(session_id, context, task_id) {
            SessionResolution::Active(_) => {}
            SessionResolution::Expired(session) => {
                self.mark_cleanup_required(&session);
                let _ = self
                    .emit(
                        AuditAction::SessionExpired,
                        context,
                        Some(&session),
                        task_id,
                        json!({}),
                    )
                    .await;
                return Ok(false);
            }
            SessionResolution::Missing | SessionResolution::ScopeMismatch => return Ok(false),
        }
        let Some(session) = self.sessions.revoke(session_id) else {
            return Ok(false);
        };
        self.mark_cleanup_required(&session);
        self.emit(
            AuditAction::SessionRevoked,
            context,
            Some(&session),
            session.scope.task_id(),
            json!({}),
        )
        .await?;
        Ok(true)
    }

    #[must_use]
    pub fn cleanup_required_for(&self, task_id: &str) -> bool {
        self.cleanup_required
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(task_id)
    }

    pub fn cleanup_finished(&self, task_id: &str) {
        self.cleanup_required
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(task_id);
    }

    fn mark_cleanup_required(&self, session: &GodSession) {
        if let SessionScope::Task(task_id) = &session.scope {
            self.cleanup_required
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(task_id.clone());
        }
    }

    async fn emit(
        &self,
        action: AuditAction,
        context: &SecurityContext,
        session: Option<&GodSession>,
        task_id: Option<&str>,
        detail: serde_json::Value,
    ) -> Result<(), GodModeError> {
        let session_ref = session.map(|session| {
            session
                .session_id
                .simple()
                .to_string()
                .chars()
                .take(8)
                .collect()
        });
        self.audit
            .emit(AuditEvent {
                action,
                occurred_at: self.clock.now(),
                owner_id: Some(context.owner_id),
                guild_id: context.guild_id,
                channel_id: Some(context.channel_id),
                task_id: task_id.map(str::to_owned),
                session_ref,
                detail,
            })
            .await
            .map_err(|error| GodModeError::Audit(error.to_string()))
    }
}

fn attempt_key(context: &SecurityContext) -> Result<AttemptKey, GodModeError> {
    Ok(AttemptKey {
        owner_id: context.owner_id,
        guild_id: context.guild_id.ok_or(AccessDenied::WrongGuild)?,
    })
}

fn context_from_session(session: &GodSession) -> SecurityContext {
    SecurityContext {
        owner_id: session.owner_id,
        guild_id: Some(session.guild_id),
        channel_id: session.channel_id,
        is_private_channel: true,
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GodModeError {
    #[error(transparent)]
    AccessDenied(#[from] AccessDenied),
    #[error("password verification failed")]
    InvalidPassword,
    #[error("password verification is temporarily locked; retry in {retry_after_seconds}s")]
    Locked { retry_after_seconds: i64 },
    #[error("security audit persistence failed: {0}")]
    Audit(String),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("pending GOD session expired before activation")]
    PendingExpired,
}
