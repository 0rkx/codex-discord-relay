use std::sync::{Arc, PoisonError, RwLock};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Clock, SecurityContext};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", content = "task_id", rename_all = "snake_case")]
pub enum SessionScope {
    Channel,
    Task(String),
}

impl SessionScope {
    #[must_use]
    pub fn task_id(&self) -> Option<&str> {
        match self {
            Self::Channel => None,
            Self::Task(task_id) => Some(task_id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GodSession {
    pub session_id: Uuid,
    pub owner_id: u64,
    pub guild_id: u64,
    pub channel_id: u64,
    pub scope: SessionScope,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionResolution {
    Active(GodSession),
    Expired(GodSession),
    Missing,
    ScopeMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionIssue {
    pub session: GodSession,
    /// Starting a new grant revokes the prior global grant, if one existed.
    pub revoked: Option<GodSession>,
}

/// Stores exactly one process-local grant. A restart or replacement revokes it.
pub struct SessionStore {
    active: RwLock<Option<GodSession>>,
    ttl: Duration,
    clock: Arc<dyn Clock>,
}

impl SessionStore {
    pub const DEFAULT_TTL: Duration = Duration::minutes(10);
    pub const MAX_TTL: Duration = Duration::minutes(10);

    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            active: RwLock::new(None),
            ttl: Self::DEFAULT_TTL,
            clock,
        }
    }

    /// # Errors
    ///
    /// Returns [`SessionError::InvalidTtl`] unless `ttl` is positive and at most ten minutes.
    pub fn with_ttl(clock: Arc<dyn Clock>, ttl: Duration) -> Result<Self, SessionError> {
        if ttl <= Duration::zero() || ttl > Self::MAX_TTL {
            return Err(SessionError::InvalidTtl);
        }
        Ok(Self {
            active: RwLock::new(None),
            ttl,
            clock,
        })
    }

    /// # Errors
    ///
    /// Returns an error when guild context or a task id required by the scope is missing.
    pub fn issue(
        &self,
        context: &SecurityContext,
        scope: SessionScope,
    ) -> Result<SessionIssue, SessionError> {
        let session = self.prepare(context, scope)?;
        Ok(self.activate(session))
    }

    /// Build a grant without making it observable. Callers can publish the
    /// mandatory warning and finish prior cleanup before activating it.
    pub fn prepare(
        &self,
        context: &SecurityContext,
        scope: SessionScope,
    ) -> Result<GodSession, SessionError> {
        let guild_id = context.guild_id.ok_or(SessionError::GuildRequired)?;
        if matches!(&scope, SessionScope::Task(task_id) if task_id.trim().is_empty()) {
            return Err(SessionError::TaskIdRequired);
        }
        let now = self.clock.now();
        let session = GodSession {
            session_id: Uuid::new_v4(),
            owner_id: context.owner_id,
            guild_id,
            channel_id: context.channel_id,
            scope,
            issued_at: now,
            expires_at: now + self.ttl,
        };
        Ok(session)
    }

    /// Atomically expose a previously prepared grant.
    pub fn activate(&self, session: GodSession) -> SessionIssue {
        let revoked = self
            .active
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(session.clone());
        SessionIssue { session, revoked }
    }

    pub fn resolve(
        &self,
        session_id: Uuid,
        context: &SecurityContext,
        task_id: Option<&str>,
    ) -> SessionResolution {
        let Some(session) = self
            .active
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return SessionResolution::Missing;
        };
        if session.session_id != session_id {
            return SessionResolution::Missing;
        }
        if self.clock.now() >= session.expires_at {
            return SessionResolution::Expired(session);
        }
        if session.owner_id != context.owner_id
            || Some(session.guild_id) != context.guild_id
            || session.channel_id != context.channel_id
            || !context.is_private_channel
        {
            return SessionResolution::ScopeMismatch;
        }
        if let SessionScope::Task(expected) = &session.scope
            && Some(expected.as_str()) != task_id
        {
            return SessionResolution::ScopeMismatch;
        }
        SessionResolution::Active(session)
    }

    /// Resolve the sole global grant without requiring Discord to retain its id.
    pub fn active_for(
        &self,
        context: &SecurityContext,
        task_id: Option<&str>,
    ) -> SessionResolution {
        let session_id = self
            .active
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|session| session.session_id);
        session_id.map_or(SessionResolution::Missing, |session_id| {
            self.resolve(session_id, context, task_id)
        })
    }

    pub fn revoke(&self, session_id: Uuid) -> Option<GodSession> {
        let mut active = self.active.write().unwrap_or_else(PoisonError::into_inner);
        if active
            .as_ref()
            .is_some_and(|session| session.session_id == session_id)
        {
            active.take()
        } else {
            None
        }
    }

    /// Emergency revocation of the only global grant, regardless of its scope.
    pub fn revoke_active(&self) -> Option<GodSession> {
        self.active
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Restore a grant after a replacement fails before it can be exposed.
    /// This is intentionally crate-private: callers must not create parallel grants.
    pub(crate) fn restore(&self, session: GodSession) {
        self.active
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(session);
    }

    pub fn purge_expired(&self) -> Vec<GodSession> {
        let now = self.clock.now();
        let expired_id = self
            .active
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|session| now >= session.expires_at)
            .map(|session| session.session_id);
        expired_id
            .and_then(|session_id| self.revoke(session_id))
            .into_iter()
            .collect()
    }

    pub fn snapshot(&self) -> Option<GodSession> {
        self.active
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn is_expired(&self, session: &GodSession) -> bool {
        self.clock.now() >= session.expires_at
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("guild context is required")]
    GuildRequired,
    #[error("task-scoped sessions require a task id")]
    TaskIdRequired,
    #[error("GOD session TTL must be positive and no more than 10 minutes")]
    InvalidTtl,
}
