//! Drives the user's native `claude` binary in `stream-json` mode and speaks
//! Claude Code's **control protocol** directly (no Node, no port), giving
//! in-process MCP and host-driven permission prompts.
//!
//! Wire format (reverse-engineered from `@anthropic-ai/claude-agent-sdk`,
//! verified against claude 2.1.183 / SDK 0.3.183 — internal & undocumented, so
//! re-verify on upgrades):
//! - CLI: `claude --output-format stream-json --verbose --input-format stream-json
//!   --permission-prompt-tool stdio [--resume <id>]`. The `--permission-prompt-tool
//!   stdio` flag is what makes the engine route tool permissions to us as
//!   `can_use_tool` control_requests.
//! - All messages are newline-delimited JSON on the child's stdio.
//! - Host→engine startup: `control_request{request_id, request:{subtype:"initialize",
//!   sdkMcpServers:["unity"], hooks:{}}}`. Listing the SDK MCP server name routes
//!   that server's calls back to us via `mcp_message`.
//! - Engine→host `control_request`:
//!   - `can_use_tool` `{tool_name, input, title, ...}` → reply a `PermissionResult`
//!     (`{behavior:"allow", updatedInput}` / `{behavior:"deny", message}`).
//!   - `mcp_message` `{server_name, message:<MCP JSON-RPC>}` → dispatch to our
//!     in-process server and reply `{mcp_response:<JSON-RPC response>}`.
//! - Reply envelope: `control_response{response:{subtype:"success"|"error",
//!   request_id, response|error}}`. Replies to *our* requests come back the same way.
//! - Stream (non-control) events: `system/init` (carries `session_id`, `model`),
//!   `assistant` (Anthropic message), `user` (tool results), `result` (turn end).
//! - Prompt input line: `{"type":"user","session_id":"","message":{"role":"user",
//!   "content":[{"type":"text","text":...}]},"parent_tool_use_id":null}`.
//!
//! A single reader thread pumps the child's stdout and never blocks: blocking work
//! (`mcp_message` → Unity tool call) is handed to short-lived helper threads, and
//! permission prompts are published for the UI and answered later via [`Driver::respond`].

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

/// Transcript field separators, mirrored by the C# parser: role-tagged blocks
/// `role{US}body` joined by `{RS}`. (ASCII record/unit separators.)
pub const RS: char = '\u{1e}';
pub const US: char = '\u{1f}';

/// Override the spawned command (default `claude`); flags are always appended.
const DEFAULT_CMD: &str = "claude";

static NEXT_REQ: AtomicU64 = AtomicU64::new(1);

// ===========================================================================
// Conversation model: serialized into the role-tagged transcript the renderer
// parses (`role{US}body` blocks joined by `{RS}`). Roles: 'u' user, 'a' agent,
// 't' thought, 'x' tool. Shared by the live reader and resume reconstruction.
// ===========================================================================

pub struct Conv {
    blocks: Vec<(char, String)>,
    tools: HashMap<String, (usize, String)>, // toolUseId -> (block index, title)
}

impl Conv {
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            tools: HashMap::new(),
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.blocks.push(('u', text.to_string()));
    }

    fn append_role(&mut self, role: char, s: &str) {
        if s.is_empty() {
            return;
        }
        if let Some((r, t)) = self.blocks.last_mut() {
            if *r == role {
                t.push_str(s);
                return;
            }
        }
        self.blocks.push((role, s.to_string()));
    }

    fn tool(&mut self, id: &str, title: &str, status: &str) {
        let glyph = match status {
            "completed" => "✓",
            "failed" => "✗",
            "in_progress" => "▸",
            _ => "·",
        };
        if let Some(entry) = self.tools.get_mut(id) {
            if !title.is_empty() {
                entry.1 = title.to_string();
            }
            let i = entry.0;
            let text = format!("{glyph} {}", entry.1);
            self.blocks[i].1 = text;
        } else {
            let label = if title.is_empty() { "(tool)" } else { title };
            let i = self.blocks.len();
            self.blocks.push(('x', format!("{glyph} {label}")));
            self.tools.insert(id.to_string(), (i, label.to_string()));
        }
    }

    fn note_closed(&mut self) {
        self.blocks.push(('a', "[connection closed]".to_string()));
    }

    /// Apply one Anthropic message's `content` (string or block array). Used for
    /// `assistant` stream events and for resume reconstruction of both roles.
    fn apply_message(&mut self, role: &str, content: &Value) {
        match content {
            Value::String(s) => {
                if role == "user" {
                    self.push_user(s);
                } else {
                    self.append_role('a', s);
                }
            }
            Value::Array(blocks) => {
                for b in blocks {
                    self.apply_block(role, b);
                }
            }
            _ => {}
        }
    }

    /// Apply only `tool_result` blocks from a `user` stream event (flipping tool
    /// glyphs). The user's own prompt is added locally on `send`, so live `user`
    /// events must NOT create a 'u' turn — hence we guard on block type here.
    fn apply_tool_results(&mut self, content: &Value) {
        if let Some(blocks) = content.as_array() {
            for b in blocks {
                if b["type"] == "tool_result" {
                    self.apply_block("user", b);
                }
            }
        }
    }

    fn apply_block(&mut self, role: &str, b: &Value) {
        match b["type"].as_str() {
            Some("text") => {
                let t = b["text"].as_str().unwrap_or("");
                if role == "user" {
                    self.push_user(t);
                } else {
                    self.append_role('a', t);
                }
            }
            Some("thinking") => self.append_role('t', b["thinking"].as_str().unwrap_or("")),
            Some("tool_use") => self.tool(
                b["id"].as_str().unwrap_or(""),
                b["name"].as_str().unwrap_or(""),
                "in_progress",
            ),
            Some("tool_result") => {
                let id = b["tool_use_id"].as_str().unwrap_or("");
                let status = if b["is_error"].as_bool().unwrap_or(false) {
                    "failed"
                } else {
                    "completed"
                };
                self.tool(id, "", status);
            }
            _ => {}
        }
    }

    pub fn serialize(&self) -> String {
        self.blocks
            .iter()
            .map(|(r, t)| format!("{r}{US}{t}"))
            .collect::<Vec<_>>()
            .join(&RS.to_string())
    }
}

/// Rebuild a [`Conv`] from a prior session's on-disk transcript, since
/// `claude --resume` retains context but does not replay turns as stream events.
/// Globs `~/.claude/projects/*/<session-id>.jsonl` (robust vs computing the
/// encoded cwd dir) and reads it best-effort (the file may not exist yet).
pub fn reconstruct_transcript(session_id: &str, _cwd: &str) -> Conv {
    let mut conv = Conv::new();
    if session_id.is_empty() {
        return conv;
    }
    // `HOME` on Unix; Windows GUI processes set `USERPROFILE` instead.
    let home = std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("USERPROFILE").ok())
        .unwrap_or_default();
    let base = PathBuf::from(home).join(".claude").join("projects");
    let file_name = format!("{session_id}.jsonl");
    let Ok(dirs) = std::fs::read_dir(&base) else {
        return conv;
    };
    for entry in dirs.flatten() {
        let path = entry.path().join(&file_name);
        if !path.is_file() {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                // Only conversational records carry a message; skip metadata
                // (queue-operation, ai-title, last-prompt, attachment, ...).
                let role = v["type"].as_str().unwrap_or("");
                if role == "user" || role == "assistant" {
                    conv.apply_message(role, &v["message"]["content"]);
                }
            }
        }
        break;
    }
    conv
}

// ===========================================================================
// Pending permission, shared driver state, and the driver itself.
// ===========================================================================

/// A `can_use_tool` request awaiting the user's decision. We synthesize the
/// allow/deny option list (the control protocol gives none) so the existing C#
/// permission UI and the session-scoped "always" memory keep working.
struct Pending {
    request_id: String,
    tool_name: String,
    input: Value,
    title: String,
}

/// State shared between the reader thread, the MCP helper threads, and the host
/// (via [`Driver`]). Everything the reader mutates lives behind a `Mutex` so the
/// UI can snapshot it from another thread without coordination.
struct State {
    writer: Mutex<ChildStdin>,
    transcript: Mutex<String>,
    status: Mutex<String>,
    pending: Mutex<Option<Pending>>,
    agent: Mutex<String>,
    session_id: Mutex<String>,
    conv: Mutex<Conv>,
    remembered: Mutex<HashMap<String, bool>>, // tool_name -> allow (session "always")
    ready: AtomicBool,
    outbox: Mutex<Vec<String>>, // prompts buffered until `initialize` completes
    init_id: String,
}

impl State {
    fn write_value(&self, v: &Value) {
        // Serialize before locking; keep the critical section to one line + flush.
        let line = format!("{v}\n");
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(line.as_bytes()).and_then(|_| w.flush());
    }

    fn write_line(&self, line: &str) {
        let mut w = self.writer.lock().unwrap();
        let _ = w
            .write_all(line.as_bytes())
            .and_then(|_| w.write_all(b"\n"))
            .and_then(|_| w.flush());
    }

    fn sync_transcript(&self) {
        let c = self.conv.lock().unwrap();
        *self.transcript.lock().unwrap() = c.serialize();
    }

    fn set_status(&self, s: &str) {
        *self.status.lock().unwrap() = s.to_string();
    }

    /// Reply to a `can_use_tool` request with the user's allow/deny decision.
    fn write_permission(&self, request_id: &str, allow: bool, input: &Value) {
        let result = if allow {
            json!({ "behavior": "allow", "updatedInput": input })
        } else {
            json!({ "behavior": "deny", "message": "User denied the request." })
        };
        self.write_value(&json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request_id, "response": result }
        }));
    }

    fn write_control_error(&self, request_id: &str, message: &str) {
        self.write_value(&json!({
            "type": "control_response",
            "response": { "subtype": "error", "request_id": request_id, "error": message }
        }));
    }
}

/// A live control-protocol session: the spawned `claude` child plus its reader
/// thread. The host calls [`Driver::send`]/[`Driver::respond`] and polls the
/// snapshot accessors; all streaming happens on the reader thread.
pub struct Driver {
    state: Arc<State>,
    child: Option<Child>,
}

impl Driver {
    /// Spawn `claude` rooted at `cwd`, send `initialize`, and start the reader.
    /// `seed` pre-populates the transcript (resume reconstruction); pass an empty
    /// [`Conv`] for a fresh session.
    pub fn new(
        cwd: String,
        resume: Option<String>,
        seed: Conv,
        claude_cmd: String,
    ) -> std::io::Result<Self> {
        // The host (see ClaudeCode) resolves `claude` to an absolute path and passes
        // it in; absent that we fall back to bare `claude` (resolved per-OS below).
        let cmd = if claude_cmd.is_empty() {
            DEFAULT_CMD.to_string()
        } else {
            claude_cmd
        };
        let mut args: Vec<String> = "--output-format stream-json --verbose \
             --input-format stream-json --permission-prompt-tool stdio"
            .split_whitespace()
            .map(String::from)
            .collect();
        if let Some(id) = resume.as_deref().filter(|s| !s.is_empty()) {
            args.push("--resume".into());
            args.push(id.to_string());
        }
        let workdir: std::path::PathBuf = if cwd.is_empty() { ".".into() } else { cwd.into() };

        // Unity launched from the GUI inherits a minimal environment, so resolve
        // `claude` the way it can be found on each OS.
        let mut command = if std::path::Path::new(&cmd).is_absolute() {
            // A known absolute path (e.g. a native install at ~/.local/bin/claude):
            // exec it directly — no shell, no rc, no PATH lookup, same on every OS.
            let mut c = Command::new(&cmd);
            c.args(&args);
            c
        } else {
            // Bare command name: resolve it on PATH the way a real terminal would.
            #[cfg(windows)]
            {
                // Windows GUI processes inherit the full user PATH; go through
                // cmd.exe so an npm `.cmd` shim resolves too, and suppress the
                // console window.
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                let mut c = Command::new("cmd");
                c.args(["/c", &format!("{cmd} {}", args.join(" "))])
                    .creation_flags(CREATE_NO_WINDOW);
                c
            }
            #[cfg(not(windows))]
            {
                // The login+interactive shell sources the user's rc so `claude`
                // resolves despite the minimal GUI PATH.
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
                let mut c = Command::new(shell);
                c.args(["-lic", &format!("exec {cmd} {}", args.join(" "))]);
                c
            }
        };
        let mut child = command
            .current_dir(workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let writer = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let init_id = format!("unterm-init-{}", NEXT_REQ.fetch_add(1, Ordering::Relaxed));

        let transcript = seed.serialize();
        let state = Arc::new(State {
            writer: Mutex::new(writer),
            transcript: Mutex::new(transcript),
            status: Mutex::new("initializing".to_string()),
            pending: Mutex::new(None),
            agent: Mutex::new(String::new()),
            session_id: Mutex::new(String::new()),
            conv: Mutex::new(seed),
            remembered: Mutex::new(HashMap::new()),
            ready: AtomicBool::new(false),
            outbox: Mutex::new(Vec::new()),
            init_id: init_id.clone(),
        });

        // Declare our in-process MCP server so the engine routes its calls to us.
        state.write_value(&json!({
            "type": "control_request",
            "request_id": init_id,
            "request": { "subtype": "initialize", "sdkMcpServers": ["unity"], "hooks": {} }
        }));

        let reader_state = state.clone();
        std::thread::spawn(move || reader_main(reader_state, stdout));

        Ok(Self {
            state,
            child: Some(child),
        })
    }

    /// Queue a user prompt. Writes immediately once `initialize` has completed,
    /// otherwise buffers until then (the engine must be initialized first).
    pub fn send(&self, prompt: &str) {
        {
            let mut c = self.state.conv.lock().unwrap();
            c.push_user(prompt);
        }
        self.state.sync_transcript();
        self.state.set_status("thinking");
        let line = user_line(prompt);
        if self.state.ready.load(Ordering::Relaxed) {
            self.state.write_line(&line);
        } else {
            self.state.outbox.lock().unwrap().push(line);
        }
    }

    /// Answer the pending permission with a synthesized option id
    /// (`allow_once`/`allow_always`/`reject_once`/`reject_always`).
    pub fn respond(&self, option_id: &str) {
        let Some(p) = self.state.pending.lock().unwrap().take() else {
            return;
        };
        let allow = option_id.starts_with("allow");
        if option_id.ends_with("always") {
            self.state
                .remembered
                .lock()
                .unwrap()
                .insert(p.tool_name.clone(), allow);
        }
        self.state.write_permission(&p.request_id, allow, &p.input);
    }

    pub fn transcript(&self) -> String {
        self.state.transcript.lock().unwrap().clone()
    }

    pub fn status(&self) -> String {
        self.state.status.lock().unwrap().clone()
    }

    pub fn agent(&self) -> String {
        self.state.agent.lock().unwrap().clone()
    }

    pub fn session_id(&self) -> String {
        self.state.session_id.lock().unwrap().clone()
    }

    /// The pending permission's title, if any (the option list is fixed).
    pub fn pending_title(&self) -> Option<String> {
        self.state.pending.lock().unwrap().as_ref().map(|p| p.title.clone())
    }

    /// Drop a pending permission without answering (used on teardown so a waiting
    /// turn doesn't keep a stale prompt up).
    pub fn clear_pending(&self) {
        *self.state.pending.lock().unwrap() = None;
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.clear_pending();
        // Kill + reap off the calling thread so a reload never blocks Unity's
        // main thread. Killing closes stdout, so the reader exits on its own.
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}

/// The stream-json input line for a user prompt.
fn user_line(text: &str) -> String {
    json!({
        "type": "user",
        "session_id": "",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
        "parent_tool_use_id": null
    })
    .to_string()
}

fn reader_main(state: Arc<State>, stdout: ChildStdout) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(v) => handle_message(&state, v),
            Err(e) => log::warn!("control: bad json line: {e}"),
        }
    }
    // Child exited / stdout closed.
    {
        let mut c = state.conv.lock().unwrap();
        c.note_closed();
    }
    state.sync_transcript();
    state.set_status("closed");
}

fn handle_message(state: &Arc<State>, v: Value) {
    match v["type"].as_str() {
        Some("control_response") => {
            // The only request we send is `initialize`; once it succeeds we are
            // ready and can flush any prompts the user queued meanwhile.
            let resp = &v["response"];
            if resp["request_id"].as_str() == Some(&state.init_id) {
                if resp["subtype"].as_str() == Some("error") {
                    let msg = resp["error"].as_str().unwrap_or("initialize failed");
                    state.set_status(&format!("init failed: {msg}"));
                    return;
                }
                state.ready.store(true, Ordering::Relaxed);
                let buffered = std::mem::take(&mut *state.outbox.lock().unwrap());
                let had_prompts = !buffered.is_empty();
                for line in buffered {
                    state.write_line(&line);
                }
                state.set_status(if had_prompts { "thinking" } else { "ready" });
            }
        }
        Some("control_request") => handle_control_request(state, &v),
        Some("system") => {
            if v["subtype"] == "init" {
                if let Some(sid) = v["session_id"].as_str() {
                    if !sid.is_empty() {
                        *state.session_id.lock().unwrap() = sid.to_string();
                    }
                }
                let mut agent = state.agent.lock().unwrap();
                if agent.is_empty() {
                    *agent = v["model"].as_str().unwrap_or("Claude Code").to_string();
                }
            }
        }
        Some("assistant") => {
            {
                let mut c = state.conv.lock().unwrap();
                c.apply_message("assistant", &v["message"]["content"]);
            }
            state.sync_transcript();
        }
        Some("user") => {
            {
                let mut c = state.conv.lock().unwrap();
                c.apply_tool_results(&v["message"]["content"]);
            }
            state.sync_transcript();
        }
        Some("result") => state.set_status("ready"),
        // keep_alive, control_cancel_request, transcript_mirror, etc.: ignore.
        _ => {}
    }
}

fn handle_control_request(state: &Arc<State>, v: &Value) {
    let request_id = v["request_id"].as_str().unwrap_or("").to_string();
    let req = &v["request"];
    match req["subtype"].as_str() {
        Some("can_use_tool") => {
            let tool_name = req["tool_name"].as_str().unwrap_or("").to_string();
            let input = req["input"].clone();
            let title = req["title"]
                .as_str()
                .or_else(|| req["display_name"].as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&tool_name)
                .to_string();
            // Auto-honor a remembered ("always") decision without bothering the UI.
            let remembered = state.remembered.lock().unwrap().get(&tool_name).copied();
            if let Some(allow) = remembered {
                state.write_permission(&request_id, allow, &input);
            } else {
                *state.pending.lock().unwrap() = Some(Pending {
                    request_id,
                    tool_name,
                    input,
                    title,
                });
            }
        }
        Some("mcp_message") => {
            // No in-process MCP server is wired in yet (added later); decline.
            state.write_control_error(&request_id, "MCP unavailable");
        }
        _ => state.write_control_error(&request_id, "unsupported control request"),
    }
}
