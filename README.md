# Unity-Unterm

A native terminal window for the Unity Editor on macOS and Windows — a real
PTY-backed shell rendered by a Rust/wgpu engine (zero-copy: IOSurface/Metal on
macOS, a shared D3D12 texture on Windows) and hosted inside an `EditorWindow`.

![Unterm running inside the Unity Editor](docs/demo.gif)

## Why

Editor work constantly bounces out to a terminal — `git`, build scripts,
tailing logs. Unterm puts a genuine terminal *inside* the editor: not a
log capture or a command runner, but a full VT emulator running your login
shell, so `vim`, `tmux`, REPLs, and TUIs all work.

## Distribution

This hardened fork is maintained as an internal dependency of Unity Cursor
Toolkit. It is not published as a standalone UPM feed. Consumers vendor the
package from an audited commit together with the native binaries and provenance
produced by this repository's CI workflow.

## Usage

Open **Window ▸ Unterm ▸ New Terminal** (`Cmd+Shift+T`). Each invocation
opens an independent terminal; open as many as you like. New terminals
start in the project root.

- **IME** — full composition input with wide-character alignment.
- **Selection** — drag to select; double/triple-click for word/line.
- **Copy / Paste** — right-click menu, or the usual editor shortcuts;
  bracketed paste is supported.
- **Scrollback** — scroll the wheel to page back through history; an overlay
  scrollbar appears on the right edge and can be dragged to any position.
- **Domain reloads** — the shell and scrollback live in the native plugin,
  so they survive C# recompiles. The window re-adopts its terminal after a
  reload instead of restarting the shell.

## Claude Code

Unterm has an in-Editor Claude Code agent panel — a transcript and composer that
drive Anthropic's standalone Claude Code engine in-process, no Node required.

1. Open **Preferences ▸ Unterm** and click **Download Claude Code**. The reviewed,
   pinned engine release is fetched from Anthropic's official npm registry into a
   per-user folder shared by all your projects. The archive is size-bounded,
   layout-validated, and verified against its platform-specific SHA-512 digest
   before installation.
2. Sign in with your own Anthropic account: run `claude login` (or type `/login`
   in the panel, which opens a terminal for the browser sign-in).
3. Open the panel from **Window ▸ Unterm ▸ Claude Code**. The menu item stays
   disabled until the engine has been downloaded.

## Code editor

Unterm can be your script editor too — an in-Editor code editor with tree-sitter
highlighting and in-process Roslyn C# completion, no external application or
solution files. Select it under **Preferences ▸ External Tools ▸ External Script
Editor ▸ Unterm Code Editor**. Afterwards, double-clicking a script, jumping to a
compile error, **Open C# Project**, and file paths clicked in the Claude Code
transcript all open there.

## Security boundary

Unity MCP tools are disabled by default. Enabling them requires confirmation in
**Preferences ▸ Unterm**. Read-only calls can then run unattended, while every
mutating or dangerous call requires a fresh Editor approval. Approval-required
calls are denied in batch mode, unknown tools fail closed, and arbitrary C#
execution is always dangerous. Claude Code permission-bypass modes are rejected.

The managed Claude child process receives an explicit environment allowlist, so
host credentials and unrelated secrets are not inherited by default.

## Platform

macOS and Windows, Unity 6.3 (6000.3) or newer. The renderer hands the editor a
GPU texture with no CPU copy — an IOSurface (Metal) on macOS, a shared D3D12
texture on Windows — so the menu item is registered only on those editors; on
any other platform the package contributes nothing.

## Repository layout

- `Packages/dev.tnayuki.unterm/` — the vendorable UPM package source. Native
  binaries are CI artifacts and are not tracked in Git.
- `native/` — the Rust source for the terminal engine. Run
  `native/build-macos.sh` (or `native/build-windows.ps1`) to build the native
  binary into the package for in-editor development. Not part of the published source.
- `provenance/` — the audited upstream revision, dependency remediation, and
  reviewed downloader pin.

Feature-branch pushes, `main`, and version tags build both native platforms,
generate an SBOM and notices, publish checksums and build metadata, and attest
the resulting evidence through GitHub Actions.

## License

MIT
