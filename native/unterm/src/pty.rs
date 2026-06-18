//! Pseudo-terminal: spawn the user's shell on a PTY and expose its master end.
//!
//! `term.rs` owns the parser/grid and drives a reader thread that pumps the
//! shell's output into it; this module just opens the PTY, launches the child,
//! and hands back the master (for resize), a reader, and a writer.

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};

pub struct Pty {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
}

/// The split halves handed out at spawn: master+child kept by the terminal,
/// reader consumed by the reader thread, writer used by the input path.
pub struct PtyHandles {
    pub pty: Pty,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

/// Spawn `shell` on a fresh PTY of `cols`x`rows`, rooted at `cwd`.
pub fn spawn(
    shell: &str,
    cwd: &str,
    cols: u16,
    rows: u16,
) -> Result<PtyHandles, Box<dyn std::error::Error>> {
    let pair = native_pty_system().openpty(PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(shell);
    if !cwd.is_empty() {
        cmd.cwd(cwd);
    }
    // Advertise a capable terminal so programs emit colors/cursor sequences.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let child = pair.slave.spawn_command(cmd)?;
    // Dropping the slave after spawn lets the child own the only slave fd, so
    // reads return EOF once it exits (otherwise the reader thread would hang).
    drop(pair.slave);

    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    Ok(PtyHandles {
        pty: Pty {
            master: pair.master,
            child,
        },
        reader,
        writer,
    })
}

impl Pty {
    /// Resize the PTY window (informs the child via SIGWINCH).
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Whether the child shell is still running.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Best-effort: ensure the shell goes away with the terminal.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
