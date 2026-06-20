//! Host-facing agent session: a thin facade over the control-protocol [`Driver`]
//! that owns the cached, NUL-terminated snapshots the C# side polls each editor
//! tick (transcript, pending permission, status, agent name, session id). All
//! streaming lives in the driver's reader thread; this layer only snapshots.

use std::ffi::CString;

use crate::control::{self, Conv, Driver, RS, US};

/// Synthesized permission options offered to the C# UI for a `can_use_tool`
/// prompt (the control protocol provides none). `(id == kind, label)`.
const PERMISSION_OPTIONS: [(&str, &str); 4] = [
    ("allow_once", "Allow"),
    ("allow_always", "Always allow"),
    ("reject_once", "Deny"),
    ("reject_always", "Always deny"),
];

pub struct AgentSession {
    driver: Option<Driver>,
    fail: String,
    snapshot: CString,
    pending_snap: CString,
    agent_snap: CString,
    status_snap: CString,
    session_id_snap: CString,
}

impl AgentSession {
    /// Start a session rooted at `cwd`, wired to the in-process MCP server.
    /// With `resume` set to a prior session id the transcript is reconstructed
    /// from disk (the engine retains context but doesn't replay it); otherwise a
    /// fresh session starts.
    pub fn new(cwd: String, resume: Option<String>, claude_cmd: String) -> Self {
        let seed = match resume.as_deref() {
            Some(id) if !id.is_empty() => control::reconstruct_transcript(id, &cwd),
            _ => Conv::new(),
        };
        let (driver, fail) = match Driver::new(cwd, resume, seed, claude_cmd) {
            Ok(d) => (Some(d), String::new()),
            Err(e) => (None, e.to_string()),
        };
        Self {
            driver,
            fail,
            snapshot: CString::default(),
            pending_snap: CString::default(),
            agent_snap: CString::default(),
            status_snap: CString::default(),
            session_id_snap: CString::default(),
        }
    }

    pub fn send(&self, prompt: &str) {
        if let Some(d) = &self.driver {
            d.send(prompt);
        }
    }

    /// Answer the pending permission with the chosen synthesized option id.
    pub fn respond(&self, option_id: &str) {
        if let Some(d) = &self.driver {
            d.respond(option_id);
        }
    }

    /// Worker status ("initializing"/"ready"/"thinking"/"closed") for FFI.
    pub fn status_snapshot(&mut self) -> &CString {
        let s = match &self.driver {
            Some(d) => d.status(),
            None => format!("spawn failed: {}", self.fail),
        };
        self.status_snap = clean(s);
        &self.status_snap
    }

    /// Snapshot the transcript into a cached, NUL-terminated buffer for FFI.
    pub fn transcript_snapshot(&mut self) -> &CString {
        let s = self.driver.as_ref().map(|d| d.transcript()).unwrap_or_default();
        self.snapshot = clean(s);
        &self.snapshot
    }

    /// Snapshot the pending permission as `title{RS}id{US}name{US}kind{RS}...`,
    /// or empty if none. Cached for FFI.
    pub fn pending_snapshot(&mut self) -> &CString {
        let s = match self.driver.as_ref().and_then(|d| d.pending_title()) {
            Some(title) => {
                let mut parts = vec![title];
                for (id, name) in PERMISSION_OPTIONS {
                    parts.push(format!("{id}{US}{name}{US}{id}"));
                }
                parts.join(&RS.to_string())
            }
            None => String::new(),
        };
        self.pending_snap = clean(s);
        &self.pending_snap
    }

    /// The running agent's display name (model id, from `system/init`).
    pub fn agent_snapshot(&mut self) -> &CString {
        let s = self.driver.as_ref().map(|d| d.agent()).unwrap_or_default();
        self.agent_snap = clean(s);
        &self.agent_snap
    }

    /// The live session id (empty until established). The host persists this to
    /// resume the conversation across editor restarts.
    pub fn session_id_snapshot(&mut self) -> &CString {
        let s = self.driver.as_ref().map(|d| d.session_id()).unwrap_or_default();
        self.session_id_snap = clean(s);
        &self.session_id_snap
    }
}

fn clean(s: String) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}
