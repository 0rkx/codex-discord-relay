//! Bounded, privacy-preserving projections for high-volume app-server events.
//!
//! This module deliberately stores only Discord-safe summaries. Commands, stdin,
//! working directories, patches, file paths, MCP arguments/results, and transport
//! payloads such as SDP never enter the projection state.

use std::collections::{HashMap, VecDeque};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

pub const MAX_DECODED_CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_COMMAND_TAIL_BYTES: usize = 16 * 1024;
pub const MAX_PROCESS_TAIL_BYTES: usize = 16 * 1024;
pub const MAX_MCP_MESSAGE_BYTES: usize = 320;
pub const MAX_MCP_MESSAGES: usize = 3;
pub const MAX_ACTIVE_THREAD_ITEMS: usize = 128;
pub const MAX_RECENT_THREAD_ITEMS: usize = 128;
pub const MAX_ACTIVE_PROCESSES: usize = 64;
pub const MAX_RECENT_PROCESSES: usize = 64;
const MAX_OPAQUE_ID_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkEncoding {
    Utf8,
    Base64,
}

#[derive(Debug, Clone, Copy)]
pub struct OutputChunk<'a> {
    pub data: &'a str,
    pub encoding: ChunkEncoding,
}

impl<'a> OutputChunk<'a> {
    #[must_use]
    pub const fn utf8(data: &'a str) -> Self {
        Self {
            data,
            encoding: ChunkEncoding::Utf8,
        }
    }

    #[must_use]
    pub const fn base64(data: &'a str) -> Self {
        Self {
            data,
            encoding: ChunkEncoding::Base64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionState {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchState {
    Running,
    Applied,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionError {
    EmptyId,
    IdTooLong,
    KindMismatch,
    InvalidBase64,
    ChunkTooLarge,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct ThreadItemKey {
    thread_id: String,
    item_id: String,
}

impl ThreadItemKey {
    fn new(thread_id: &str, item_id: &str) -> Result<Self, ProjectionError> {
        Ok(Self {
            thread_id: checked_id(thread_id)?.to_owned(),
            item_id: checked_id(item_id)?.to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutputTail {
    pub stdout: String,
    pub stderr: String,
    pub capped: bool,
    pub incomplete: bool,
    pub lagged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandProjection {
    pub output: OutputTail,
    pub active: bool,
    pub completion: Option<CompletionState>,
    pub exit_code: Option<i32>,
    touched: u64,
}

impl CommandProjection {
    fn new(touched: u64) -> Self {
        Self {
            output: OutputTail::default(),
            active: true,
            completion: None,
            exit_code: None,
            touched,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProgressProjection {
    pub messages: VecDeque<String>,
    pub active: bool,
    pub completion: Option<CompletionState>,
    pub incomplete: bool,
    pub lagged: bool,
    touched: u64,
}

impl McpProgressProjection {
    fn new(touched: u64) -> Self {
        Self {
            messages: VecDeque::new(),
            active: true,
            completion: None,
            incomplete: false,
            lagged: false,
            touched,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchProjection {
    pub state: PatchState,
    pub files_changed: Option<u32>,
    pub hunks_applied: Option<u32>,
    pub hunks_total: Option<u32>,
    pub incomplete: bool,
    pub lagged: bool,
    touched: u64,
}

impl PatchProjection {
    fn new(touched: u64) -> Self {
        Self {
            state: PatchState::Running,
            files_changed: None,
            hunks_applied: None,
            hunks_total: None,
            incomplete: false,
            lagged: false,
            touched,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessProjection {
    pub output: OutputTail,
    pub active: bool,
    pub completion: Option<CompletionState>,
    pub exit_code: Option<i32>,
    touched: u64,
}

impl ProcessProjection {
    fn new(touched: u64) -> Self {
        Self {
            output: OutputTail::default(),
            active: true,
            completion: None,
            exit_code: None,
            touched,
        }
    }
}

#[derive(Debug, Clone)]
enum ThreadProjection {
    Command(CommandProjection),
    Mcp(McpProgressProjection),
    Patch(PatchProjection),
}

impl ThreadProjection {
    fn active(&self) -> bool {
        match self {
            Self::Command(value) => value.active,
            Self::Mcp(value) => value.active,
            Self::Patch(value) => value.state == PatchState::Running,
        }
    }

    fn touched(&self) -> u64 {
        match self {
            Self::Command(value) => value.touched,
            Self::Mcp(value) => value.touched,
            Self::Patch(value) => value.touched,
        }
    }

    fn touch(&mut self, now: u64) {
        match self {
            Self::Command(value) => value.touched = now,
            Self::Mcp(value) => value.touched = now,
            Self::Patch(value) => value.touched = now,
        }
    }

    fn mark_lagged(&mut self) {
        match self {
            Self::Command(value) => mark_output_lagged(&mut value.output),
            Self::Mcp(value) => {
                value.lagged = true;
                value.incomplete = true;
            }
            Self::Patch(value) => {
                value.lagged = true;
                value.incomplete = true;
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct ProjectionCore {
    thread_items: HashMap<ThreadItemKey, ThreadProjection>,
    processes: HashMap<String, ProcessProjection>,
    clock: u64,
}

impl ProjectionCore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_command_output(
        &mut self,
        thread_id: &str,
        item_id: &str,
        stream: OutputStream,
        chunk: OutputChunk<'_>,
    ) -> Result<(), ProjectionError> {
        let key = ThreadItemKey::new(thread_id, item_id)?;
        let now = self.tick();
        let entry = self
            .thread_items
            .entry(key)
            .or_insert_with(|| ThreadProjection::Command(CommandProjection::new(now)));
        entry.touch(now);
        let ThreadProjection::Command(command) = entry else {
            return Err(ProjectionError::KindMismatch);
        };
        command.active = true;
        match decode_chunk(chunk) {
            Ok(text) => append_output(&mut command.output, stream, &text, MAX_COMMAND_TAIL_BYTES),
            Err(error) => {
                command.output.capped |= error == ProjectionError::ChunkTooLarge;
                command.output.incomplete = true;
                self.prune_thread_items();
                return Err(error);
            }
        }
        self.prune_thread_items();
        Ok(())
    }

    pub fn complete_command(
        &mut self,
        thread_id: &str,
        item_id: &str,
        state: CompletionState,
        exit_code: Option<i32>,
        authoritative_stdout: Option<OutputChunk<'_>>,
        authoritative_stderr: Option<OutputChunk<'_>>,
    ) -> Result<(), ProjectionError> {
        let key = ThreadItemKey::new(thread_id, item_id)?;
        let now = self.tick();
        let entry = self
            .thread_items
            .entry(key)
            .or_insert_with(|| ThreadProjection::Command(CommandProjection::new(now)));
        let ThreadProjection::Command(command) = entry else {
            return Err(ProjectionError::KindMismatch);
        };
        command.touched = now;
        command.active = false;
        command.completion = Some(state);
        command.exit_code = exit_code;
        if let Err(error) = heal_output(
            &mut command.output,
            authoritative_stdout,
            authoritative_stderr,
            MAX_COMMAND_TAIL_BYTES,
        ) {
            command.output.capped |= error == ProjectionError::ChunkTooLarge;
            command.output.incomplete = true;
            self.prune_thread_items();
            return Err(error);
        }
        self.prune_thread_items();
        Ok(())
    }

    pub fn push_mcp_progress(
        &mut self,
        thread_id: &str,
        item_id: &str,
        message: &str,
    ) -> Result<(), ProjectionError> {
        let key = ThreadItemKey::new(thread_id, item_id)?;
        let now = self.tick();
        let entry = self
            .thread_items
            .entry(key)
            .or_insert_with(|| ThreadProjection::Mcp(McpProgressProjection::new(now)));
        entry.touch(now);
        let ThreadProjection::Mcp(progress) = entry else {
            return Err(ProjectionError::KindMismatch);
        };
        progress.active = true;
        let message = bounded_render_text(message, MAX_MCP_MESSAGE_BYTES);
        if !message.is_empty() {
            progress.messages.retain(|prior| prior != &message);
            progress.messages.push_back(message);
            while progress.messages.len() > MAX_MCP_MESSAGES {
                progress.messages.pop_front();
            }
        }
        self.prune_thread_items();
        Ok(())
    }

    pub fn complete_mcp(
        &mut self,
        thread_id: &str,
        item_id: &str,
        state: CompletionState,
        authoritative_message: Option<&str>,
    ) -> Result<(), ProjectionError> {
        if let Some(message) = authoritative_message {
            self.push_mcp_progress(thread_id, item_id, message)?;
        }
        let key = ThreadItemKey::new(thread_id, item_id)?;
        let now = self.tick();
        let entry = self
            .thread_items
            .entry(key)
            .or_insert_with(|| ThreadProjection::Mcp(McpProgressProjection::new(now)));
        let ThreadProjection::Mcp(progress) = entry else {
            return Err(ProjectionError::KindMismatch);
        };
        progress.touched = now;
        progress.active = false;
        progress.completion = Some(state);
        if authoritative_message.is_some() {
            progress.incomplete = false;
            progress.lagged = false;
        }
        self.prune_thread_items();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_patch(
        &mut self,
        thread_id: &str,
        item_id: &str,
        state: PatchState,
        files_changed: Option<u32>,
        hunks_applied: Option<u32>,
        hunks_total: Option<u32>,
        authoritative: bool,
    ) -> Result<(), ProjectionError> {
        let key = ThreadItemKey::new(thread_id, item_id)?;
        let now = self.tick();
        let entry = self
            .thread_items
            .entry(key)
            .or_insert_with(|| ThreadProjection::Patch(PatchProjection::new(now)));
        let ThreadProjection::Patch(patch) = entry else {
            return Err(ProjectionError::KindMismatch);
        };
        patch.touched = now;
        patch.state = state;
        patch.files_changed = files_changed.or(patch.files_changed);
        patch.hunks_applied = hunks_applied.or(patch.hunks_applied);
        patch.hunks_total = hunks_total.or(patch.hunks_total);
        if authoritative {
            patch.incomplete = false;
            patch.lagged = false;
        }
        self.prune_thread_items();
        Ok(())
    }

    pub fn push_process_output(
        &mut self,
        process_id: &str,
        stream: OutputStream,
        chunk: OutputChunk<'_>,
    ) -> Result<(), ProjectionError> {
        let process_id = checked_id(process_id)?.to_owned();
        let now = self.tick();
        let process = self
            .processes
            .entry(process_id)
            .or_insert_with(|| ProcessProjection::new(now));
        process.touched = now;
        process.active = true;
        match decode_chunk(chunk) {
            Ok(text) => append_output(&mut process.output, stream, &text, MAX_PROCESS_TAIL_BYTES),
            Err(error) => {
                process.output.capped |= error == ProjectionError::ChunkTooLarge;
                process.output.incomplete = true;
                self.prune_processes();
                return Err(error);
            }
        }
        self.prune_processes();
        Ok(())
    }

    pub fn complete_process(
        &mut self,
        process_id: &str,
        state: CompletionState,
        exit_code: Option<i32>,
        authoritative_stdout: Option<OutputChunk<'_>>,
        authoritative_stderr: Option<OutputChunk<'_>>,
    ) -> Result<(), ProjectionError> {
        let process_id = checked_id(process_id)?.to_owned();
        let now = self.tick();
        let process = self
            .processes
            .entry(process_id)
            .or_insert_with(|| ProcessProjection::new(now));
        process.touched = now;
        process.active = false;
        process.completion = Some(state);
        process.exit_code = exit_code;
        if let Err(error) = heal_output(
            &mut process.output,
            authoritative_stdout,
            authoritative_stderr,
            MAX_PROCESS_TAIL_BYTES,
        ) {
            process.output.capped |= error == ProjectionError::ChunkTooLarge;
            process.output.incomplete = true;
            self.prune_processes();
            return Err(error);
        }
        self.prune_processes();
        Ok(())
    }

    #[cfg(test)]
    pub fn mark_thread_lagged(&mut self, thread_id: &str) {
        for (key, value) in &mut self.thread_items {
            if key.thread_id == thread_id && value.active() {
                value.mark_lagged();
            }
        }
    }

    pub fn mark_all_threads_lagged(&mut self) {
        for value in self
            .thread_items
            .values_mut()
            .filter(|value| value.active())
        {
            value.mark_lagged();
        }
    }

    pub fn mark_connection_lagged(&mut self) {
        for process in self.processes.values_mut().filter(|value| value.active) {
            mark_output_lagged(&mut process.output);
        }
    }

    #[must_use]
    pub fn command(&self, thread_id: &str, item_id: &str) -> Option<&CommandProjection> {
        match self.thread_items.get(&ThreadItemKey {
            thread_id: thread_id.to_owned(),
            item_id: item_id.to_owned(),
        })? {
            ThreadProjection::Command(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub fn mcp(&self, thread_id: &str, item_id: &str) -> Option<&McpProgressProjection> {
        match self.thread_items.get(&ThreadItemKey {
            thread_id: thread_id.to_owned(),
            item_id: item_id.to_owned(),
        })? {
            ThreadProjection::Mcp(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub fn patch(&self, thread_id: &str, item_id: &str) -> Option<&PatchProjection> {
        match self.thread_items.get(&ThreadItemKey {
            thread_id: thread_id.to_owned(),
            item_id: item_id.to_owned(),
        })? {
            ThreadProjection::Patch(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub fn process(&self, process_id: &str) -> Option<&ProcessProjection> {
        self.processes.get(process_id)
    }

    #[cfg(test)]
    #[must_use]
    pub fn thread_item_count(&self) -> usize {
        self.thread_items.len()
    }

    #[cfg(test)]
    #[must_use]
    pub fn process_count(&self) -> usize {
        self.processes.len()
    }

    fn tick(&mut self) -> u64 {
        self.clock = self.clock.saturating_add(1);
        self.clock
    }

    fn prune_thread_items(&mut self) {
        prune_map(
            &mut self.thread_items,
            MAX_ACTIVE_THREAD_ITEMS,
            MAX_RECENT_THREAD_ITEMS,
            ThreadProjection::active,
            ThreadProjection::touched,
        );
    }

    fn prune_processes(&mut self) {
        prune_map(
            &mut self.processes,
            MAX_ACTIVE_PROCESSES,
            MAX_RECENT_PROCESSES,
            |value| value.active,
            |value| value.touched,
        );
    }
}

fn checked_id(value: &str) -> Result<&str, ProjectionError> {
    if value.is_empty() {
        Err(ProjectionError::EmptyId)
    } else if value.len() > MAX_OPAQUE_ID_BYTES {
        Err(ProjectionError::IdTooLong)
    } else {
        Ok(value)
    }
}

fn decode_chunk(chunk: OutputChunk<'_>) -> Result<String, ProjectionError> {
    match chunk.encoding {
        ChunkEncoding::Utf8 => {
            if chunk.data.len() > MAX_DECODED_CHUNK_BYTES {
                return Err(ProjectionError::ChunkTooLarge);
            }
            Ok(chunk.data.to_owned())
        }
        ChunkEncoding::Base64 => {
            let encoded_len = chunk.data.len();
            if !encoded_len.is_multiple_of(4) || !chunk.data.is_ascii() {
                return Err(ProjectionError::InvalidBase64);
            }
            let predicted = encoded_len
                .checked_div(4)
                .and_then(|groups| groups.checked_mul(3))
                .ok_or(ProjectionError::ChunkTooLarge)?;
            if predicted > MAX_DECODED_CHUNK_BYTES.saturating_add(2) {
                return Err(ProjectionError::ChunkTooLarge);
            }
            let bytes = BASE64_STANDARD
                .decode(chunk.data)
                .map_err(|_| ProjectionError::InvalidBase64)?;
            if bytes.len() > MAX_DECODED_CHUNK_BYTES {
                return Err(ProjectionError::ChunkTooLarge);
            }
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}

fn heal_output(
    output: &mut OutputTail,
    stdout: Option<OutputChunk<'_>>,
    stderr: Option<OutputChunk<'_>>,
    cap: usize,
) -> Result<(), ProjectionError> {
    let has_authoritative = stdout.is_some() || stderr.is_some();
    let stdout = stdout.map(decode_chunk).transpose()?;
    let stderr = stderr.map(decode_chunk).transpose()?;
    if has_authoritative {
        output.capped = false;
    }
    if let Some(decoded) = stdout {
        output.stdout.clear();
        output.capped |= set_bounded_tail(&mut output.stdout, &decoded, cap);
    }
    if let Some(decoded) = stderr {
        output.stderr.clear();
        output.capped |= set_bounded_tail(&mut output.stderr, &decoded, cap);
    }
    if has_authoritative {
        output.incomplete = false;
        output.lagged = false;
    }
    Ok(())
}

fn append_output(output: &mut OutputTail, stream: OutputStream, text: &str, cap: usize) {
    let target = match stream {
        OutputStream::Stdout => &mut output.stdout,
        OutputStream::Stderr => &mut output.stderr,
    };
    let sanitized = sanitize_render_text(text);
    target.push_str(&sanitized);
    if target.len() > cap {
        trim_to_tail(target, cap);
        output.capped = true;
    }
}

fn set_bounded_tail(target: &mut String, text: &str, cap: usize) -> bool {
    target.push_str(&sanitize_render_text(text));
    if target.len() <= cap {
        false
    } else {
        trim_to_tail(target, cap);
        true
    }
}

fn mark_output_lagged(output: &mut OutputTail) {
    output.lagged = true;
    output.incomplete = true;
}

fn trim_to_tail(value: &mut String, cap: usize) {
    let mut start = value.len().saturating_sub(cap);
    while !value.is_char_boundary(start) {
        start += 1;
    }
    value.drain(..start);
}

#[must_use]
pub fn bounded_render_text(value: &str, cap: usize) -> String {
    let mut result = sanitize_render_text(value);
    if result.len() > cap {
        trim_to_tail(&mut result, cap);
    }
    result
}

/// Removes terminal escape/control sequences and obvious path/credential shapes.
/// The result is the only text retained by this module.
#[must_use]
pub fn sanitize_render_text(value: &str) -> String {
    let secret_redacted =
        crate::security::redact_secrets(&serde_json::Value::String(value.to_owned()));
    let secret_redacted = secret_redacted.as_str().unwrap_or("[redacted]");
    let controls_removed = strip_terminal_controls(secret_redacted);
    let mut result = String::with_capacity(controls_removed.len());
    for segment in controls_removed.split_inclusive('\n') {
        if let Some(line) = segment.strip_suffix('\n') {
            result.push_str(&redact_line(line));
            result.push('\n');
        } else {
            result.push_str(&redact_line(segment));
        }
    }
    result
}

fn strip_terminal_controls(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    let mut escaped = false;
                    for next in chars.by_ref() {
                        if next == '\u{7}' || (escaped && next == '\\') {
                            break;
                        }
                        escaped = next == '\u{1b}';
                    }
                }
                _ => {}
            }
            continue;
        }
        if ch == '\n' || ch == '\t' || !ch.is_control() {
            result.push(ch);
        }
    }
    result
}

fn redact_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let lowercase = trimmed.to_ascii_lowercase();
    if let Some(separator) = lowercase.find(['=', ':']) {
        let key = lowercase[..separator].trim();
        if is_secret_key(key) {
            return format!("{}[redacted]", &line[..line.len() - trimmed.len()]);
        }
    }
    line.split_inclusive(char::is_whitespace)
        .map(redact_token)
        .collect()
}

fn is_secret_key(key: &str) -> bool {
    [
        "authorization",
        "password",
        "passwd",
        "secret",
        "token",
        "api_key",
        "apikey",
        "cookie",
        "private_key",
    ]
    .iter()
    .any(|candidate| key.ends_with(candidate))
}

fn redact_token(token: &str) -> String {
    let core = token.trim_matches(|ch: char| ch.is_ascii_punctuation() && ch != '/' && ch != '\\');
    let lower = core.to_ascii_lowercase();
    let secret_assignment = lower
        .find(['=', ':'])
        .is_some_and(|separator| is_secret_key(lower[..separator].trim()));
    let looks_path = core.starts_with('/')
        || core.starts_with("\\\\")
        || (core.len() >= 3
            && core.as_bytes()[1] == b':'
            && matches!(core.as_bytes()[2], b'\\' | b'/'));
    let looks_secret = secret_assignment
        || lower.starts_with("sk-")
        || lower.starts_with("ghp_")
        || lower.starts_with("github_pat_")
        || lower.starts_with("bearer")
        || (core.matches('.').count() == 2 && core.len() > 40);
    if looks_path {
        preserve_whitespace(token, "[path]")
    } else if looks_secret {
        preserve_whitespace(token, "[redacted]")
    } else {
        token.to_owned()
    }
}

fn preserve_whitespace(original: &str, replacement: &str) -> String {
    let suffix = &original[original.trim_end().len()..];
    format!("{replacement}{suffix}")
}

fn prune_map<K, V, A, T>(
    map: &mut HashMap<K, V>,
    active_limit: usize,
    recent_limit: usize,
    is_active: A,
    touched: T,
) where
    K: Clone + Eq + std::hash::Hash,
    A: Fn(&V) -> bool,
    T: Fn(&V) -> u64,
{
    loop {
        let active = map.values().filter(|value| is_active(value)).count();
        let recent = map.len().saturating_sub(active);
        let evict_active = active > active_limit;
        let evict_recent = recent > recent_limit;
        if !evict_active && !evict_recent {
            break;
        }
        let key = map
            .iter()
            .filter(|(_, value)| is_active(value) == evict_active)
            .min_by_key(|(_, value)| touched(value))
            .map(|(key, _)| key.clone());
        if let Some(key) = key {
            map.remove(&key);
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_output_is_keyed_by_thread_and_item() {
        let mut core = ProjectionCore::new();
        core.push_command_output("a", "x", OutputStream::Stdout, OutputChunk::utf8("one"))
            .unwrap();
        core.push_command_output("b", "x", OutputStream::Stdout, OutputChunk::utf8("two"))
            .unwrap();
        assert_eq!(core.command("a", "x").unwrap().output.stdout, "one");
        assert_eq!(core.command("b", "x").unwrap().output.stdout, "two");
    }

    #[test]
    fn a_thread_item_cannot_silently_change_projection_kind() {
        let mut core = ProjectionCore::new();
        core.push_mcp_progress("t", "same", "working").unwrap();
        assert_eq!(
            core.push_command_output(
                "t",
                "same",
                OutputStream::Stdout,
                OutputChunk::utf8("unexpected")
            ),
            Err(ProjectionError::KindMismatch)
        );
        assert!(core.command("t", "same").is_none());
    }

    #[test]
    fn base64_is_preflighted_and_decoded() {
        let mut core = ProjectionCore::new();
        core.push_process_output("p", OutputStream::Stdout, OutputChunk::base64("aGVsbG8="))
            .unwrap();
        assert_eq!(core.process("p").unwrap().output.stdout, "hello");
        assert_eq!(
            core.push_process_output("p", OutputStream::Stdout, OutputChunk::base64("%%%")),
            Err(ProjectionError::InvalidBase64)
        );
        assert!(core.process("p").unwrap().output.incomplete);
    }

    #[test]
    fn oversized_chunk_is_rejected_without_allocating_decode_buffer() {
        let mut core = ProjectionCore::new();
        let encoded = "A".repeat((MAX_DECODED_CHUNK_BYTES / 3 + 2) * 4);
        assert_eq!(
            core.push_process_output("p", OutputStream::Stdout, OutputChunk::base64(&encoded)),
            Err(ProjectionError::ChunkTooLarge)
        );
        let output = &core.process("p").unwrap().output;
        assert!(output.capped && output.incomplete);
    }

    #[test]
    fn output_tail_is_utf8_safe_and_bounded() {
        let mut core = ProjectionCore::new();
        let text = "🙂".repeat(MAX_DECODED_CHUNK_BYTES / 4 + 1);
        assert_eq!(
            core.push_command_output("t", "i", OutputStream::Stdout, OutputChunk::utf8(&text)),
            Err(ProjectionError::ChunkTooLarge)
        );
        let smaller = "🙂".repeat(MAX_DECODED_CHUNK_BYTES / 4);
        core.push_command_output("t", "j", OutputStream::Stdout, OutputChunk::utf8(&smaller))
            .unwrap();
        let output = &core.command("t", "j").unwrap().output;
        assert!(output.stdout.len() <= MAX_COMMAND_TAIL_BYTES);
        assert!(output.stdout.is_char_boundary(0));
        assert!(output.capped);
    }

    #[test]
    fn terminal_controls_paths_and_secrets_are_not_retained() {
        let dirty = "\u{1b}[31mred\u{1b}[0m C:\\Users\\person\\key token=abc\nAuthorization: Bearer xyz\n/usr/bin/tool sk-live-secret";
        let clean = sanitize_render_text(dirty);
        assert!(!clean.contains('\u{1b}'));
        assert!(!clean.contains("Users"));
        assert!(!clean.contains("abc"));
        assert!(!clean.contains("Bearer xyz"));
        assert!(!clean.contains("/usr"));
        assert!(!clean.contains("sk-live"));
        assert!(clean.contains("red"));
    }

    #[test]
    fn chunk_newlines_are_preserved() {
        let mut core = ProjectionCore::new();
        core.push_command_output("t", "i", OutputStream::Stdout, OutputChunk::utf8("one\n"))
            .unwrap();
        core.push_command_output("t", "i", OutputStream::Stdout, OutputChunk::utf8("two\n"))
            .unwrap();
        assert_eq!(core.command("t", "i").unwrap().output.stdout, "one\ntwo\n");
    }

    #[test]
    fn mcp_retains_last_three_distinct_safe_messages() {
        let mut core = ProjectionCore::new();
        for message in ["one", "two", "three", "two", "four"] {
            core.push_mcp_progress("t", "m", message).unwrap();
        }
        let messages = &core.mcp("t", "m").unwrap().messages;
        assert_eq!(
            messages.iter().cloned().collect::<Vec<_>>(),
            ["three", "two", "four"]
        );
    }

    #[test]
    fn mcp_message_is_bounded_and_does_not_store_raw_arguments() {
        let mut core = ProjectionCore::new();
        let message = format!("{} /private/file", "x".repeat(500));
        core.push_mcp_progress("t", "m", &message).unwrap();
        let stored = core.mcp("t", "m").unwrap().messages.back().unwrap();
        assert!(stored.len() <= MAX_MCP_MESSAGE_BYTES);
        assert!(!stored.contains("/private"));
    }

    #[test]
    fn patch_projection_contains_counts_but_no_paths_or_patch_text() {
        let mut core = ProjectionCore::new();
        core.update_patch(
            "t",
            "f",
            PatchState::Running,
            Some(2),
            Some(3),
            Some(5),
            false,
        )
        .unwrap();
        let patch = core.patch("t", "f").unwrap();
        assert_eq!(patch.files_changed, Some(2));
        assert_eq!(patch.hunks_applied, Some(3));
        assert_eq!(patch.hunks_total, Some(5));
    }

    #[test]
    fn lag_marks_only_active_thread_items_and_can_be_healed() {
        let mut core = ProjectionCore::new();
        core.push_command_output("t", "a", OutputStream::Stdout, OutputChunk::utf8("partial"))
            .unwrap();
        core.complete_command("t", "b", CompletionState::Succeeded, Some(0), None, None)
            .unwrap();
        core.mark_thread_lagged("t");
        assert!(core.command("t", "a").unwrap().output.lagged);
        assert!(!core.command("t", "b").unwrap().output.lagged);
        core.complete_command(
            "t",
            "a",
            CompletionState::Succeeded,
            Some(0),
            Some(OutputChunk::utf8("authoritative")),
            Some(OutputChunk::utf8("")),
        )
        .unwrap();
        let output = &core.command("t", "a").unwrap().output;
        assert_eq!(output.stdout, "authoritative");
        assert!(!output.lagged && !output.incomplete);
    }

    #[test]
    fn authoritative_mcp_and_patch_updates_heal_lag() {
        let mut core = ProjectionCore::new();
        core.push_mcp_progress("t", "m", "working").unwrap();
        core.update_patch("t", "p", PatchState::Running, None, None, None, false)
            .unwrap();
        core.mark_thread_lagged("t");
        core.complete_mcp("t", "m", CompletionState::Succeeded, Some("done"))
            .unwrap();
        core.update_patch(
            "t",
            "p",
            PatchState::Applied,
            Some(1),
            Some(1),
            Some(1),
            true,
        )
        .unwrap();
        assert!(!core.mcp("t", "m").unwrap().lagged);
        assert!(!core.patch("t", "p").unwrap().lagged);
    }

    #[test]
    fn process_streams_and_completion_are_connection_scoped() {
        let mut core = ProjectionCore::new();
        core.push_process_output("opaque", OutputStream::Stdout, OutputChunk::utf8("out"))
            .unwrap();
        core.push_process_output("opaque", OutputStream::Stderr, OutputChunk::utf8("err"))
            .unwrap();
        core.complete_process("opaque", CompletionState::Failed, Some(9), None, None)
            .unwrap();
        let process = core.process("opaque").unwrap();
        assert_eq!(process.output.stdout, "out");
        assert_eq!(process.output.stderr, "err");
        assert_eq!(process.exit_code, Some(9));
        assert!(!process.active);
    }

    #[test]
    fn connection_lag_is_healed_by_authoritative_process_completion() {
        let mut core = ProjectionCore::new();
        core.push_process_output("p", OutputStream::Stdout, OutputChunk::utf8("partial"))
            .unwrap();
        core.mark_connection_lagged();
        assert!(core.process("p").unwrap().output.incomplete);
        core.complete_process(
            "p",
            CompletionState::Succeeded,
            Some(0),
            Some(OutputChunk::utf8("full")),
            Some(OutputChunk::utf8("")),
        )
        .unwrap();
        assert!(!core.process("p").unwrap().output.incomplete);
    }

    #[test]
    fn active_and_recent_thread_items_are_separately_bounded() {
        let mut core = ProjectionCore::new();
        for index in 0..=MAX_ACTIVE_THREAD_ITEMS {
            core.push_mcp_progress("t", &format!("a{index}"), "x")
                .unwrap();
        }
        assert_eq!(core.thread_item_count(), MAX_ACTIVE_THREAD_ITEMS);
        assert!(core.mcp("t", "a0").is_none());
        for index in 0..=MAX_RECENT_THREAD_ITEMS {
            core.complete_command(
                "t",
                &format!("r{index}"),
                CompletionState::Succeeded,
                Some(0),
                None,
                None,
            )
            .unwrap();
        }
        assert_eq!(
            core.thread_item_count(),
            MAX_ACTIVE_THREAD_ITEMS + MAX_RECENT_THREAD_ITEMS
        );
        assert!(core.command("t", "r0").is_none());
    }

    #[test]
    fn active_and_recent_processes_are_separately_bounded() {
        let mut core = ProjectionCore::new();
        for index in 0..=MAX_ACTIVE_PROCESSES {
            core.push_process_output(
                &format!("a{index}"),
                OutputStream::Stdout,
                OutputChunk::utf8("x"),
            )
            .unwrap();
        }
        assert!(core.process("a0").is_none());
        for index in 0..=MAX_RECENT_PROCESSES {
            core.complete_process(
                &format!("r{index}"),
                CompletionState::Succeeded,
                Some(0),
                None,
                None,
            )
            .unwrap();
        }
        assert_eq!(
            core.process_count(),
            MAX_ACTIVE_PROCESSES + MAX_RECENT_PROCESSES
        );
        assert!(core.process("r0").is_none());
    }

    #[test]
    fn opaque_ids_are_validated_and_never_exposed_in_projection_payloads() {
        let mut core = ProjectionCore::new();
        assert_eq!(
            core.push_process_output("", OutputStream::Stdout, OutputChunk::utf8("x")),
            Err(ProjectionError::EmptyId)
        );
        let long = "x".repeat(MAX_OPAQUE_ID_BYTES + 1);
        assert_eq!(
            core.push_process_output(&long, OutputStream::Stdout, OutputChunk::utf8("x")),
            Err(ProjectionError::IdTooLong)
        );
    }
}
