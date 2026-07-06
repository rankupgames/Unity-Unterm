//! Headless end-to-end check of the git-diff gutter pipeline: a real temp repo with
//! a staged file, an editor whose buffer differs from the index, then the async
//! fetch → poll → render path. Verifies the background git read is delivered and a
//! render with diff markers doesn't panic across the FFI. No pixel readback (the
//! editor draws to a shared MTLTexture), so this is a wiring/crash smoke, not a
//! visual diff. Run: `cargo run -p unterm --example diff_smoke`

use std::ffi::{CStr, CString};
use std::path::Path;
use std::time::{Duration, Instant};

use unterm::*;

/// Poll the background git fetch until it's applied (or time out).
fn wait_diff(id: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if unterm_editor_poll_diff(id) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The editor's current buffer text via the FFI.
fn editor_text(id: u64) -> String {
    let mut len = 0usize;
    let p = unsafe { unterm_editor_text(id, &mut len) };
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn main() {
    env_logger::try_init().ok();

    // A temp git repo with foo.cs COMMITTED at three lines (HEAD is the diff base).
    let dir = std::env::temp_dir().join(format!("unterm-diff-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let repo = git2::Repository::init(&dir).unwrap();
    let file = dir.join("foo.cs");
    std::fs::write(&file, "class A {\n    int x;\n    int y;\n}\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("foo.cs")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();

    let id = unterm_editor_create(900, 500, 2.0);
    assert!(id != 0, "editor create failed");
    let lang = CString::new("cs").unwrap();
    unsafe { unterm_editor_set_language(id, lang.as_ptr()) };

    // Buffer differs from the index: line 2 modified, a new line added, line "y" gone.
    let buf = CString::new("class A {\n    int x = 1;\n    int z;\n}\n").unwrap();
    unsafe { unterm_editor_set_text(id, buf.as_ptr()) };

    // Point at the staged file → kicks the background git fetch.
    let path = CString::new(file.to_string_lossy().to_string()).unwrap();
    unsafe { unterm_editor_set_path(id, path.as_ptr()) };

    // Poll until the fetched base is applied (background thread), with a timeout.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut applied = false;
    while Instant::now() < deadline {
        if unterm_editor_poll_diff(id) {
            applied = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(applied, "git base was never delivered by poll_diff");
    println!("poll_diff delivered the git base OK");

    // Render with markers active — must not panic across the FFI.
    unterm_editor_render(id);
    let ch = unterm_editor_content_height(id);
    assert!(ch > 0.0, "content height should be positive after render");
    println!("render with diff markers OK (content_height={ch})");

    // Hover the gutter marker of the modified line (index 1) to show the peek tooltip,
    // then render — exercises line_at_y / hunk lookup / tooltip overlay without panic.
    // scale 2.0 → line_height 40, pad 12: line 1 sits around y≈52..92.
    let shown = unterm_editor_hover(id, 6.0, 72.0);
    assert!(shown, "hover over a modified line's gutter marker should show the tooltip");
    unterm_editor_render(id);
    println!("gutter-marker hover + tooltip render OK");
    // Moving off the marker hides the tooltip: the first away-hover returns true (a
    // repaint is needed to clear it), and a second one returns false (nothing shown).
    assert!(unterm_editor_hover(id, 400.0, 300.0), "away-hover should request a clear repaint");
    assert!(!unterm_editor_hover(id, 400.0, 300.0), "tooltip should now be hidden");
    unterm_editor_render(id);
    println!("tooltip hide (hover away) + render OK");

    // Refresh (focus / periodic poll path) with UNCHANGED git texts: the delivery
    // must be a no-op (poll_diff false), so the steady-state 1s refresh never
    // forces re-renders or drops the peek.
    unterm_editor_refresh_diff(id);
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        assert!(!unterm_editor_poll_diff(id), "unchanged refresh must not report a change");
        std::thread::sleep(Duration::from_millis(10));
    }
    unterm_editor_render(id);
    println!("refresh with unchanged texts is a no-op OK");

    // --- hunk_at + STAGE (HEAD base: the marker must SURVIVE staging, hollow) ---
    let hi = unterm_editor_hunk_at(id, 6.0, 72.0);
    assert!(hi >= 0, "hunk_at should find the modified hunk in the gutter");
    assert!(!unterm_editor_hunk_staged(id, hi as u32), "hunk should start unstaged");
    println!("hunk_at found hunk {hi} (unstaged)");
    assert!(unterm_editor_stage_hunk(id, hi as u32), "stage_hunk should succeed");
    // Only hunk here == the whole change, so the index blob should now equal the buffer.
    let repo2 = git2::Repository::discover(&dir).unwrap();
    let entry = repo2.index().unwrap().get_path(Path::new("foo.cs"), 0).unwrap();
    let staged = String::from_utf8(repo2.find_blob(entry.id).unwrap().content().to_vec()).unwrap();
    assert_eq!(staged, "class A {\n    int x = 1;\n    int z;\n}\n", "index updated by stage_hunk");
    println!("stage_hunk updated the index OK");

    // Pick up the refreshed git texts: the hunk is still there (buffer != HEAD) but
    // now reads staged, and its marker draws hollow.
    wait_diff(id);
    unterm_editor_render(id);
    let hi2 = unterm_editor_hunk_at(id, 6.0, 72.0);
    assert!(hi2 >= 0, "marker must survive staging (HEAD base)");
    assert!(unterm_editor_hunk_staged(id, hi2 as u32), "hunk should now read staged");
    println!("marker survives staging and reads staged OK");

    // --- UNSTAGE: the index goes back to HEAD, the hunk reads unstaged again ---
    assert!(unterm_editor_unstage_hunk(id, hi2 as u32), "unstage_hunk should succeed");
    let entry = {
        let repo3 = git2::Repository::discover(&dir).unwrap();
        repo3.index().unwrap().get_path(Path::new("foo.cs"), 0).unwrap()
    };
    let repo3 = git2::Repository::discover(&dir).unwrap();
    let unstaged = String::from_utf8(repo3.find_blob(entry.id).unwrap().content().to_vec()).unwrap();
    assert_eq!(unstaged, "class A {\n    int x;\n    int y;\n}\n", "index restored to HEAD");
    wait_diff(id);
    unterm_editor_render(id);
    let hi3 = unterm_editor_hunk_at(id, 6.0, 72.0);
    assert!(hi3 >= 0 && !unterm_editor_hunk_staged(id, hi3 as u32), "hunk reads unstaged again");
    println!("unstage_hunk restored the index OK");

    // --- STAGED-ONLY: stage, then revert the buffer back to HEAD. The change now
    // lives only in the index (`git diff --cached` shows it) — the editor must keep
    // showing a (hollow, staged) hunk there and allow unstaging it.
    assert!(unterm_editor_stage_hunk(id, hi3 as u32), "re-stage should succeed");
    wait_diff(id);
    let head_buf = CString::new("class A {\n    int x;\n    int y;\n}\n").unwrap();
    unsafe { unterm_editor_set_text(id, head_buf.as_ptr()) }; // buffer back at HEAD
    unterm_editor_render(id);
    let so = unterm_editor_hunk_at(id, 6.0, 72.0);
    assert!(so >= 0, "staged-only hunk must still show a marker");
    assert!(unterm_editor_hunk_staged(id, so as u32), "staged-only hunk reads staged");
    assert!(unterm_editor_hover(id, 6.0, 72.0), "staged-only hunk is peekable");
    unterm_editor_render(id);
    assert!(unterm_editor_unstage_hunk(id, so as u32), "unstage staged-only should succeed");
    wait_diff(id);
    unterm_editor_render(id);
    assert!(unterm_editor_hunk_at(id, 6.0, 72.0) < 0, "everything clean → no markers");
    println!("staged-only hunk shown, peeked, and unstaged OK");

    unterm_editor_destroy(id);
    std::fs::remove_dir_all(&dir).unwrap();

    // --- REVERT (fresh repo/editor so the state is independent of staging) ---
    let dir2 = std::env::temp_dir().join(format!("unterm-diff-smoke-b-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir2);
    std::fs::create_dir_all(&dir2).unwrap();
    let repo_b = git2::Repository::init(&dir2).unwrap();
    let file2 = dir2.join("bar.cs");
    std::fs::write(&file2, "class B {\n    int p;\n}\n").unwrap();
    let mut ib = repo_b.index().unwrap();
    ib.add_path(Path::new("bar.cs")).unwrap();
    ib.write().unwrap();

    let id2 = unterm_editor_create(900, 500, 2.0);
    unsafe { unterm_editor_set_language(id2, lang.as_ptr()) };
    let buf2 = CString::new("class B {\n    int q;\n}\n").unwrap(); // line 1 modified
    unsafe { unterm_editor_set_text(id2, buf2.as_ptr()) };
    let path2 = CString::new(file2.to_string_lossy().to_string()).unwrap();
    unsafe { unterm_editor_set_path(id2, path2.as_ptr()) };
    wait_diff(id2);
    unterm_editor_render(id2); // compute hunks

    let hi2 = unterm_editor_hunk_at(id2, 6.0, 72.0);
    assert!(hi2 >= 0, "hunk_at should find the modified hunk (revert)");
    unterm_editor_revert_hunk(id2, hi2 as u32);
    assert_eq!(editor_text(id2), "class B {\n    int p;\n}\n", "revert restored the base content");
    println!("revert_hunk restored the base content OK");

    // --- pure ADDITION is peekable (VS Code parity: its peek shows the + lines) ---
    // Insert a line between "int p;" and "}": a pure-added hunk at line 2
    // (y≈92..132 at scale 2: pad 12 + 2*40).
    let buf3 = CString::new("class B {\n    int p;\n    int r;\n}\n").unwrap();
    unsafe { unterm_editor_set_text(id2, buf3.as_ptr()) };
    unterm_editor_render(id2);
    assert!(
        unterm_editor_hover(id2, 6.0, 112.0),
        "hovering a pure-addition marker should show its + lines tooltip"
    );
    unterm_editor_render(id2);
    println!("pure-addition hover tooltip OK");

    unterm_editor_destroy(id2);
    std::fs::remove_dir_all(&dir2).unwrap();
    println!("diff_smoke PASSED");
}
