use std::path::Path;

use anyhow::Result;
use chrono::{Duration, Utc};
use sqlx::{pool::Pool, query::query, row::Row};
use sqlx_sqlite::{Sqlite, SqliteConnectOptions, SqliteJournalMode, SqliteRow};

use crate::models::{TaskMirror, TaskState};

type SqlitePool = Pool<Sqlite>;

#[derive(Clone)]
pub struct StateStore {
    pool: SqlitePool,
}

impl StateStore {
    pub async fn connect(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePool::connect_with(options).await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<()> {
        for statement in SCHEMA.split(";\n").map(str::trim).filter(|s| !s.is_empty()) {
            query(statement).execute(&self.pool).await?;
        }
        self.ensure_column("task_mirrors", "model", "TEXT").await?;
        self.ensure_column("pending_requests", "channel_id", "INTEGER")
            .await?;
        self.ensure_column("pending_requests", "message_id", "INTEGER")
            .await?;
        self.ensure_column("pending_requests", "rpc_id_json", "TEXT")
            .await?;
        Ok(())
    }

    async fn ensure_column(&self, table: &str, column: &str, kind: &str) -> Result<()> {
        let rows = query(&format!("PRAGMA table_info({table})"))
            .fetch_all(&self.pool)
            .await?;
        let exists = rows
            .iter()
            .any(|row| row.get::<String, _>("name") == column);
        if !exists {
            query(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    pub async fn upsert_task(&self, task: &TaskMirror) -> Result<()> {
        let now = Utc::now();
        query(
            r#"INSERT INTO task_mirrors
               (thread_id, channel_id, title, cwd, state, turn_id, model, last_event_at, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(thread_id) DO UPDATE SET
                 channel_id=excluded.channel_id, title=excluded.title, cwd=excluded.cwd,
                 state=excluded.state, turn_id=excluded.turn_id, model=excluded.model,
                 last_event_at=excluded.last_event_at, updated_at=excluded.updated_at"#,
        )
        .bind(&task.thread_id)
        .bind(task.channel_id.map(|id| id as i64))
        .bind(&task.title)
        .bind(&task.cwd)
        .bind(state_str(task.state))
        .bind(&task.turn_id)
        .bind(&task.model)
        .bind(task.last_event_at)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn task_by_channel(&self, channel_id: u64) -> Result<Option<TaskMirror>> {
        let row = query("SELECT * FROM task_mirrors WHERE channel_id = ?")
            .bind(channel_id as i64)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| row_to_task(&row)).transpose()
    }

    pub async fn task(&self, thread_id: &str) -> Result<Option<TaskMirror>> {
        let row = query("SELECT * FROM task_mirrors WHERE thread_id = ?")
            .bind(thread_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| row_to_task(&row)).transpose()
    }

    pub async fn detach_channel(&self, thread_id: &str) -> Result<()> {
        query("UPDATE task_mirrors SET channel_id = NULL, updated_at = ? WHERE thread_id = ?")
            .bind(Utc::now())
            .bind(thread_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn detach_channel_id(&self, channel_id: u64) -> Result<()> {
        query("UPDATE task_mirrors SET channel_id = NULL, updated_at = ? WHERE channel_id = ?")
            .bind(Utc::now())
            .bind(channel_id as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn tasks(&self) -> Result<Vec<TaskMirror>> {
        query("SELECT * FROM task_mirrors ORDER BY updated_at DESC")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(row_to_task)
            .collect()
    }

    pub async fn set_cursor(&self, channel_id: u64, message_id: u64) -> Result<()> {
        query(
            r#"INSERT INTO channel_cursors(channel_id, last_message_id, updated_at)
               VALUES (?, ?, ?)
               ON CONFLICT(channel_id) DO UPDATE SET
                 last_message_id=MAX(last_message_id, excluded.last_message_id),
                 updated_at=excluded.updated_at"#,
        )
        .bind(channel_id as i64)
        .bind(message_id as i64)
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn cursor(&self, channel_id: u64) -> Result<Option<u64>> {
        let row = query("SELECT last_message_id FROM channel_cursors WHERE channel_id = ?")
            .bind(channel_id as i64)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<i64, _>(0) as u64))
    }

    pub async fn audit(
        &self,
        event_type: &str,
        actor_id: Option<u64>,
        guild_id: Option<u64>,
        channel_id: Option<u64>,
        thread_id: Option<&str>,
        detail: &serde_json::Value,
    ) -> Result<()> {
        self.audit_with_retention(
            event_type,
            actor_id,
            guild_id,
            channel_id,
            thread_id,
            detail,
            Self::AUDIT_MAX_ROWS,
            Self::AUDIT_MAX_AGE,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn audit_with_retention(
        &self,
        event_type: &str,
        actor_id: Option<u64>,
        guild_id: Option<u64>,
        channel_id: Option<u64>,
        thread_id: Option<&str>,
        detail: &serde_json::Value,
        max_rows: u32,
        max_age: Duration,
    ) -> Result<()> {
        let now = Utc::now();
        let mut transaction = self.pool.begin().await?;
        query(
            r#"INSERT INTO audit_events
               (event_type, actor_id, guild_id, channel_id, thread_id, detail_json, created_at)
               VALUES (?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(event_type)
        .bind(actor_id.map(|id| id as i64))
        .bind(guild_id.map(|id| id as i64))
        .bind(channel_id.map(|id| id as i64))
        .bind(thread_id)
        .bind(detail.to_string())
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        query("DELETE FROM audit_events WHERE created_at < ?")
            .bind(now - max_age)
            .execute(&mut *transaction)
            .await?;
        query(
            r#"DELETE FROM audit_events
               WHERE id <= COALESCE(
                 (SELECT id FROM audit_events ORDER BY id DESC LIMIT 1 OFFSET ?),
                 -1
               )"#,
        )
        .bind(i64::from(max_rows))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn save_pending_request(
        &self,
        request_id: &str,
        thread_id: &str,
        turn_id: Option<&str>,
        method: &str,
        params: &serde_json::Value,
        rpc_id: &serde_json::Value,
    ) -> Result<()> {
        query(
            r#"INSERT INTO pending_requests
               (request_id, thread_id, turn_id, method, params_json, rpc_id_json, created_at)
               VALUES (?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(request_id) DO UPDATE SET
                 thread_id=excluded.thread_id, turn_id=excluded.turn_id,
                 method=excluded.method, params_json=excluded.params_json,
                 rpc_id_json=excluded.rpc_id_json"#,
        )
        .bind(request_id)
        .bind(thread_id)
        .bind(turn_id)
        .bind(method)
        .bind(params.to_string())
        .bind(rpc_id.to_string())
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_pending_request_message(
        &self,
        request_id: &str,
        channel_id: u64,
        message_id: u64,
    ) -> Result<()> {
        query("UPDATE pending_requests SET channel_id = ?, message_id = ? WHERE request_id = ?")
            .bind(channel_id as i64)
            .bind(message_id as i64)
            .bind(request_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn pending_requests(&self) -> Result<Vec<PendingRequestRow>> {
        let rows = query(
            "SELECT request_id, thread_id, method, channel_id, message_id FROM pending_requests ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(PendingRequestRow {
                    request_id: row.try_get("request_id")?,
                    thread_id: row.try_get("thread_id")?,
                    method: row.try_get("method")?,
                    channel_id: row
                        .try_get::<Option<i64>, _>("channel_id")?
                        .map(|id| id as u64),
                    message_id: row
                        .try_get::<Option<i64>, _>("message_id")?
                        .map(|id| id as u64),
                })
            })
            .collect()
    }

    pub async fn remove_pending_request(&self, request_id: &str) -> Result<()> {
        query("DELETE FROM pending_requests WHERE request_id = ?")
            .bind(request_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn enqueue_outbox(
        &self,
        dedupe_key: &str,
        channel_id: u64,
        kind: &str,
        payload: &serde_json::Value,
    ) -> Result<()> {
        query(
            r#"INSERT INTO outbox(dedupe_key, channel_id, kind, payload_json, attempts, created_at)
               VALUES (?, ?, ?, ?, 0, ?)
               ON CONFLICT(dedupe_key) DO NOTHING"#,
        )
        .bind(dedupe_key)
        .bind(channel_id as i64)
        .bind(kind)
        .bind(payload.to_string())
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn pending_outbox(&self, limit: u32) -> Result<Vec<OutboxRow>> {
        let rows = query(
            "SELECT id, dedupe_key, channel_id, kind, payload_json, attempts FROM outbox WHERE sent_at IS NULL AND attempts < ? ORDER BY id LIMIT ?",
        )
        .bind(i64::from(Self::MAX_OUTBOX_ATTEMPTS))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(OutboxRow {
                    id: row.try_get("id")?,
                    dedupe_key: row.try_get("dedupe_key")?,
                    channel_id: row.try_get::<i64, _>("channel_id")? as u64,
                    kind: row.try_get("kind")?,
                    payload: serde_json::from_str(&row.try_get::<String, _>("payload_json")?)?,
                    attempts: row.try_get::<i64, _>("attempts")? as u32,
                })
            })
            .collect()
    }

    pub async fn mark_outbox_sent(&self, id: i64) -> Result<()> {
        query("UPDATE outbox SET sent_at = ? WHERE id = ?")
            .bind(Utc::now())
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.prune_outbox().await?;
        Ok(())
    }

    pub async fn mark_outbox_attempt(&self, id: i64, error: &str) -> Result<()> {
        query("UPDATE outbox SET attempts = attempts + 1, last_error = ? WHERE id = ?")
            .bind(error.chars().take(1000).collect::<String>())
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.prune_outbox().await?;
        Ok(())
    }

    pub async fn dead_outbox_count(&self) -> Result<u64> {
        let row = query("SELECT COUNT(*) AS n FROM outbox WHERE sent_at IS NULL AND attempts >= ?")
            .bind(i64::from(Self::MAX_OUTBOX_ATTEMPTS))
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("n") as u64)
    }

    async fn prune_outbox(&self) -> Result<()> {
        let sent_cutoff = Utc::now() - Duration::days(1);
        let dead_cutoff = Utc::now() - Duration::days(30);
        let mut tx = self.pool.begin().await?;
        query("DELETE FROM outbox WHERE sent_at IS NOT NULL AND sent_at < ?")
            .bind(sent_cutoff)
            .execute(&mut *tx)
            .await?;
        query("DELETE FROM outbox WHERE sent_at IS NULL AND attempts >= ? AND created_at < ?")
            .bind(i64::from(Self::MAX_OUTBOX_ATTEMPTS))
            .bind(dead_cutoff)
            .execute(&mut *tx)
            .await?;
        query(
            "DELETE FROM outbox WHERE id NOT IN (SELECT id FROM outbox ORDER BY id DESC LIMIT ?)",
        )
        .bind(i64::from(Self::OUTBOX_MAX_ROWS))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn mark_god_dirty(&self, thread_id: &str, reason: &str) -> Result<()> {
        query(
            r#"INSERT INTO god_dirty_tasks(thread_id, reason, marked_at)
               VALUES (?, ?, ?)
               ON CONFLICT(thread_id) DO UPDATE SET reason = excluded.reason, marked_at = excluded.marked_at"#,
        )
        .bind(thread_id)
        .bind(reason.chars().take(1000).collect::<String>())
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_god_dirty(&self, thread_id: &str) -> Result<()> {
        query("DELETE FROM god_dirty_tasks WHERE thread_id = ?")
            .bind(thread_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn god_dirty_tasks(&self) -> Result<Vec<(String, String)>> {
        let rows = query("SELECT thread_id, reason FROM god_dirty_tasks ORDER BY marked_at")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| Ok((row.try_get("thread_id")?, row.try_get("reason")?)))
            .collect()
    }

    pub async fn queue_sensitive_deletion(&self, channel_id: u64, message_id: u64) -> Result<()> {
        query(
            r#"INSERT INTO sensitive_deletions(channel_id, message_id, attempts, created_at)
               VALUES (?, ?, 0, ?)
               ON CONFLICT(channel_id, message_id) DO NOTHING"#,
        )
        .bind(channel_id as i64)
        .bind(message_id as i64)
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn pending_sensitive_deletions(&self, limit: u32) -> Result<Vec<SensitiveDeletion>> {
        let newest_limit = limit.div_ceil(2);
        let oldest_limit = limit / 2;
        let rows = query(
            r"WITH newest AS (
                   SELECT channel_id, message_id, attempts, created_at
                   FROM sensitive_deletions
                   ORDER BY created_at DESC
                   LIMIT ?
               ), oldest AS (
                   SELECT channel_id, message_id, attempts, created_at
                   FROM sensitive_deletions
                   ORDER BY created_at ASC
                   LIMIT ?
               )
               SELECT channel_id, message_id, attempts
               FROM (SELECT * FROM newest UNION SELECT * FROM oldest)
               ORDER BY attempts ASC, created_at DESC
               LIMIT ?",
        )
        .bind(i64::from(newest_limit))
        .bind(i64::from(oldest_limit))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(SensitiveDeletion {
                    channel_id: row.try_get::<i64, _>("channel_id")? as u64,
                    message_id: row.try_get::<i64, _>("message_id")? as u64,
                    attempts: row.try_get::<i64, _>("attempts")? as u32,
                })
            })
            .collect()
    }

    pub async fn finish_sensitive_deletion(&self, channel_id: u64, message_id: u64) -> Result<()> {
        query("DELETE FROM sensitive_deletions WHERE channel_id = ? AND message_id = ?")
            .bind(channel_id as i64)
            .bind(message_id as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn fail_sensitive_deletion(
        &self,
        channel_id: u64,
        message_id: u64,
        error: &str,
    ) -> Result<()> {
        query(
            "UPDATE sensitive_deletions SET attempts = attempts + 1, last_error = ? WHERE channel_id = ? AND message_id = ?",
        )
        .bind(error.chars().take(1000).collect::<String>())
        .bind(channel_id as i64)
        .bind(message_id as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub const MAX_OUTBOX_ATTEMPTS: u32 = 10;
    pub const OUTBOX_MAX_ROWS: u32 = 10_000;
    pub const AUDIT_MAX_ROWS: u32 = 100_000;
    pub const AUDIT_MAX_AGE: Duration = Duration::days(30);
}

#[derive(Debug, Clone)]
pub struct PendingRequestRow {
    pub request_id: String,
    pub thread_id: String,
    pub method: String,
    pub channel_id: Option<u64>,
    pub message_id: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct OutboxRow {
    pub id: i64,
    pub dedupe_key: String,
    pub channel_id: u64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub attempts: u32,
}

#[derive(Debug, Clone)]
pub struct SensitiveDeletion {
    pub channel_id: u64,
    pub message_id: u64,
    pub attempts: u32,
}

fn row_to_task(row: &SqliteRow) -> Result<TaskMirror> {
    Ok(TaskMirror {
        thread_id: row.try_get("thread_id")?,
        channel_id: row
            .try_get::<Option<i64>, _>("channel_id")?
            .map(|id| id as u64),
        title: row.try_get("title")?,
        cwd: row.try_get("cwd")?,
        state: parse_state(row.try_get("state")?)?,
        turn_id: row.try_get("turn_id")?,
        model: row.try_get("model")?,
        last_event_at: row.try_get("last_event_at")?,
    })
}

const fn state_str(state: TaskState) -> &'static str {
    match state {
        TaskState::Running => "running",
        TaskState::NeedsUser => "needs_user",
        TaskState::Done => "done",
        TaskState::Failed => "failed",
        TaskState::Idle => "idle",
    }
}

fn parse_state(value: String) -> Result<TaskState> {
    Ok(match value.as_str() {
        "running" => TaskState::Running,
        "needs_user" => TaskState::NeedsUser,
        "done" => TaskState::Done,
        "failed" => TaskState::Failed,
        "idle" => TaskState::Idle,
        other => anyhow::bail!("unknown task state {other}"),
    })
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS task_mirrors (
  thread_id TEXT PRIMARY KEY,
  channel_id INTEGER UNIQUE,
  title TEXT NOT NULL,
  cwd TEXT,
  state TEXT NOT NULL,
  turn_id TEXT,
  model TEXT,
  last_event_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS task_mirrors_state_idx ON task_mirrors(state, updated_at);
CREATE TABLE IF NOT EXISTS channel_cursors (
  channel_id INTEGER PRIMARY KEY,
  last_message_id INTEGER NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS pending_requests (
  request_id TEXT PRIMARY KEY,
  thread_id TEXT NOT NULL,
  turn_id TEXT,
  method TEXT NOT NULL,
  params_json TEXT NOT NULL,
  rpc_id_json TEXT,
  channel_id INTEGER,
  message_id INTEGER,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS audit_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  event_type TEXT NOT NULL,
  actor_id INTEGER,
  guild_id INTEGER,
  channel_id INTEGER,
  thread_id TEXT,
  detail_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS audit_events_created_at_idx ON audit_events(created_at, id);
CREATE TABLE IF NOT EXISTS outbox (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  dedupe_key TEXT NOT NULL UNIQUE,
  channel_id INTEGER NOT NULL,
  kind TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  created_at TEXT NOT NULL,
  sent_at TEXT
);
CREATE TABLE IF NOT EXISTS god_dirty_tasks (
  thread_id TEXT PRIMARY KEY,
  reason TEXT NOT NULL,
  marked_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sensitive_deletions (
  channel_id INTEGER NOT NULL,
  message_id INTEGER NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  created_at TEXT NOT NULL,
  PRIMARY KEY(channel_id, message_id)
);
"#;

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn task_and_cursor_round_trip() {
        let dir = tempdir().unwrap();
        let store = StateStore::connect(&dir.path().join("state.sqlite3"))
            .await
            .unwrap();
        let task = TaskMirror {
            thread_id: "thr-1".into(),
            channel_id: Some(42),
            title: "Build".into(),
            cwd: Some("C:/work".into()),
            state: TaskState::Running,
            turn_id: None,
            model: Some("gpt-5.1-codex".into()),
            last_event_at: None,
        };
        store.upsert_task(&task).await.unwrap();
        assert_eq!(
            store
                .task_by_channel(42)
                .await
                .unwrap()
                .unwrap()
                .model
                .as_deref(),
            Some("gpt-5.1-codex")
        );
        assert_eq!(
            store.task_by_channel(42).await.unwrap().unwrap().thread_id,
            "thr-1"
        );
        store.detach_channel_id(42).await.unwrap();
        assert!(store.task_by_channel(42).await.unwrap().is_none());
        assert_eq!(store.task("thr-1").await.unwrap().unwrap().channel_id, None);
        store.set_cursor(42, 100).await.unwrap();
        store.set_cursor(42, 99).await.unwrap();
        assert_eq!(store.cursor(42).await.unwrap(), Some(100));
    }

    #[tokio::test]
    async fn pending_card_location_survives_restart() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.sqlite3");
        let store = StateStore::connect(&path).await.unwrap();
        store
            .save_pending_request(
                "req-1",
                "thr-1",
                Some("turn-1"),
                "item/commandExecution/requestApproval",
                &serde_json::json!({"command":"cargo test"}),
                &serde_json::json!(42),
            )
            .await
            .unwrap();
        store
            .set_pending_request_message("req-1", 7, 9)
            .await
            .unwrap();
        drop(store);

        let reopened = StateStore::connect(&path).await.unwrap();
        let rows = reopened.pending_requests().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].channel_id, Some(7));
        assert_eq!(rows[0].message_id, Some(9));
    }

    #[tokio::test]
    async fn poisoned_outbox_row_becomes_a_dead_letter() {
        let dir = tempdir().unwrap();
        let store = StateStore::connect(&dir.path().join("state.sqlite3"))
            .await
            .unwrap();
        store
            .enqueue_outbox("key-1", 5, "answer_page", &serde_json::json!({"text":"x"}))
            .await
            .unwrap();
        for _ in 0..StateStore::MAX_OUTBOX_ATTEMPTS {
            let row = store.pending_outbox(1).await.unwrap().remove(0);
            store
                .mark_outbox_attempt(row.id, "channel deleted")
                .await
                .unwrap();
        }
        assert!(store.pending_outbox(10).await.unwrap().is_empty());
        assert_eq!(store.dead_outbox_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn audit_retention_prunes_old_rows_and_keeps_newest_rows() {
        let dir = tempdir().unwrap();
        let store = StateStore::connect(&dir.path().join("state.sqlite3"))
            .await
            .unwrap();
        query("INSERT INTO audit_events(event_type, detail_json, created_at) VALUES (?, ?, ?)")
            .bind("expired")
            .bind("{}")
            .bind(Utc::now() - Duration::days(10))
            .execute(&store.pool)
            .await
            .unwrap();

        for sequence in 1..=5 {
            store
                .audit_with_retention(
                    &format!("event-{sequence}"),
                    None,
                    None,
                    None,
                    None,
                    &serde_json::json!({"sequence": sequence}),
                    3,
                    Duration::days(1),
                )
                .await
                .unwrap();
        }

        let rows = query("SELECT event_type FROM audit_events ORDER BY id")
            .fetch_all(&store.pool)
            .await
            .unwrap();
        let retained = rows
            .iter()
            .map(|row| row.get::<String, _>("event_type"))
            .collect::<Vec<_>>();
        assert_eq!(retained, ["event-3", "event-4", "event-5"]);
    }

    #[tokio::test]
    async fn god_dirty_marker_survives_restart_until_cleanup() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.sqlite3");
        let store = StateStore::connect(&path).await.unwrap();
        store
            .mark_god_dirty("thr-god", "privileged turn accepted")
            .await
            .unwrap();
        drop(store);

        let reopened = StateStore::connect(&path).await.unwrap();
        assert_eq!(
            reopened.god_dirty_tasks().await.unwrap(),
            vec![("thr-god".to_owned(), "privileged turn accepted".to_owned())]
        );
        reopened.clear_god_dirty("thr-god").await.unwrap();
        assert!(reopened.god_dirty_tasks().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn sensitive_deletion_retry_survives_restart_without_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.sqlite3");
        let store = StateStore::connect(&path).await.unwrap();
        store.queue_sensitive_deletion(7, 9).await.unwrap();
        store
            .fail_sensitive_deletion(7, 9, "temporary Discord failure")
            .await
            .unwrap();
        drop(store);

        let reopened = StateStore::connect(&path).await.unwrap();
        let rows = reopened.pending_sensitive_deletions(10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].channel_id, rows[0].message_id, rows[0].attempts),
            (7, 9, 1)
        );
        reopened.finish_sensitive_deletion(7, 9).await.unwrap();
        assert!(
            reopened
                .pending_sensitive_deletions(10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn sensitive_deletion_batch_serves_new_and_old_rows() {
        let dir = tempdir().unwrap();
        let store = StateStore::connect(&dir.path().join("state.sqlite3"))
            .await
            .unwrap();
        for message_id in 1..=60 {
            store.queue_sensitive_deletion(7, message_id).await.unwrap();
            store
                .fail_sensitive_deletion(7, message_id, "stale Discord failure")
                .await
                .unwrap();
        }
        store.queue_sensitive_deletion(7, 999).await.unwrap();

        let rows = store.pending_sensitive_deletions(10).await.unwrap();
        assert_eq!(rows.len(), 10);
        assert!(rows.iter().any(|row| row.message_id == 999));
        assert!(rows.iter().any(|row| row.message_id <= 5));
    }
}
