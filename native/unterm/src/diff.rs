//! Git-diff line markers for the code editor, modeled on VS Code / Zed:
//!
//! * The gutter diffs the buffer against **HEAD**, so a change stays marked until
//!   it's committed — staging doesn't make it vanish. [`hunks`] produces the changed
//!   blocks, [`markers_from_hunks`] the per-line marker byte, both recomputed once
//!   per edit in [`crate::input::InputBox::render`], not per frame.
//! * Which of those hunks are **staged** comes from the diff-of-diffs (Zed's model):
//!   diff(HEAD → index) is compared with diff(HEAD → buffer) by [`staged_flags`];
//!   a hunk whose exact change is already in the index draws hollow.
//! * [`stage_apply`] / [`unstage_apply`] compute the new index content for staging /
//!   unstaging one hunk (like `git add -p` / `git restore --staged -p`), which
//!   [`stage_blob`] writes back as the stage-0 blob.
//! * [`DiffFetcher`] runs the `git2` reads on a background thread so a large repo
//!   never stalls the editor, and hands the fetched texts back on a later poll.
//!
//! All texts (via [`normalize_newlines`]; the buffer is LF-normalized on load by the
//! C# host) are in `\n` line-space, so indices line up with the buffer's logical
//! lines (`text().split('\n')`).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

/// Per-current-line marker bits, OR-combined (a line can be both modified and have
/// a deletion just above it). Indexed by logical line (0-based) of the live buffer.
pub const ADDED: u8 = 1;
pub const MODIFIED: u8 = 2;
/// Lines were removed from the base immediately *before* this line.
pub const DELETED_ABOVE: u8 = 4;
/// Lines were removed at end-of-file, *after* the last line (drawn below it).
pub const DELETED_BELOW: u8 = 8;
/// The line's added/modified hunk is staged (its bar draws hollow).
pub const STAGED: u8 = 16;
/// The deletion at this line's boundary is staged (its wedge draws hollow).
pub const STAGED_DEL: u8 = 32;

/// CRLF/CR → LF, matching the host's on-load normalization so an autocrlf repo
/// doesn't show every line as modified.
pub fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// One changed block between the base and the current buffer, in both line spaces.
/// `old_len == 0` is a pure addition (nothing removed to peek); `new_len == 0` is a
/// pure deletion (removed lines sit at the `new_start` boundary in the current buffer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
}

/// The changed blocks diffing `base` (HEAD — or the index when there's no commit
/// yet) against `cur` (live buffer). Both are LF line-space. Empty `base` → no
/// hunks (untracked / diff off).
pub fn hunks(base: &str, cur: &str) -> Vec<Hunk> {
    if base.is_empty() {
        return Vec::new();
    }
    let base_lines: Vec<&str> = base.split('\n').collect();
    let cur_lines: Vec<&str> = cur.split('\n').collect();

    use similar::{capture_diff_slices, Algorithm, DiffOp};
    let mut out = Vec::new();
    for op in capture_diff_slices(Algorithm::Myers, &base_lines, &cur_lines) {
        let h = match op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => Hunk {
                old_start: old_index,
                old_len: 0,
                new_start: new_index,
                new_len,
            },
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => Hunk {
                old_start: old_index,
                old_len,
                new_start: new_index,
                new_len: 0,
            },
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => Hunk {
                old_start: old_index,
                old_len,
                new_start: new_index,
                new_len,
            },
        };
        out.push(h);
    }
    out
}

/// Per-current-line marker byte derived from `hunks`, for a buffer of `n` lines.
pub fn markers_from_hunks(hunks: &[Hunk], n: usize) -> Vec<u8> {
    let mut marks = vec![0u8; n];
    for h in hunks {
        if h.new_len > 0 {
            // Added lines, or a replaced run shown as "modified" for its whole new
            // span (like VS Code; any surplus removed base lines fold into it).
            let bit = if h.old_len > 0 { MODIFIED } else { ADDED };
            for m in marks
                .iter_mut()
                .take((h.new_start + h.new_len).min(n))
                .skip(h.new_start)
            {
                *m |= bit;
            }
        } else {
            // Pure deletion: a boundary marker where the removed text sat (or below
            // the last line when it fell off the end).
            if h.new_start < n {
                marks[h.new_start] |= DELETED_ABOVE;
            } else if n > 0 {
                marks[n - 1] |= DELETED_BELOW;
            }
        }
    }
    marks
}

/// Marker byte per current-buffer line, diffing `base` against `cur`. Convenience
/// over [`hunks`] + [`markers_from_hunks`] (which the editor uses directly, to also
/// keep the hunks for peeking); the one-call form the diff tests read against.
#[cfg(test)]
fn line_markers(base: &str, cur: &str) -> Vec<u8> {
    let n = cur.split('\n').count();
    markers_from_hunks(&hunks(base, cur), n)
}

/// Which of `cur_hunks` (diff HEAD → buffer) are already staged: a hunk is staged
/// when the index contains the exact same change — an `index_hunks` (diff HEAD →
/// index) entry with the same HEAD range whose index lines equal the buffer lines.
/// A hunk edited further after staging reads as unstaged again (VS Code-like: the
/// working change is what the marker tracks).
pub fn staged_flags(index: &str, index_hunks: &[Hunk], cur: &str, cur_hunks: &[Hunk]) -> Vec<bool> {
    let il: Vec<&str> = index.split('\n').collect();
    let cl: Vec<&str> = cur.split('\n').collect();
    let slice = |lines: &[&str], start: usize, len: usize| -> Option<Vec<String>> {
        let end = start.checked_add(len)?;
        (end <= lines.len()).then(|| lines[start..end].iter().map(|s| s.to_string()).collect())
    };
    cur_hunks
        .iter()
        .map(|h| {
            index_hunks.iter().any(|ih| {
                ih.old_start == h.old_start
                    && ih.old_len == h.old_len
                    && ih.new_len == h.new_len
                    && slice(&il, ih.new_start, ih.new_len) == slice(&cl, h.new_start, h.new_len)
            })
        })
        .collect()
}

/// OR the [`STAGED`] / [`STAGED_DEL`] bits into `marks` for each staged hunk, using
/// the same line/boundary mapping as [`markers_from_hunks`].
pub fn apply_staged_bits(marks: &mut [u8], hunks: &[Hunk], staged: &[bool]) {
    let n = marks.len();
    for (h, st) in hunks.iter().zip(staged) {
        if !st {
            continue;
        }
        if h.new_len > 0 {
            for m in marks
                .iter_mut()
                .take((h.new_start + h.new_len).min(n))
                .skip(h.new_start)
            {
                *m |= STAGED;
            }
        } else if h.new_start < n {
            marks[h.new_start] |= STAGED_DEL;
        } else if n > 0 {
            marks[n - 1] |= STAGED_DEL;
        }
    }
}

/// Inclusive interval touch (an empty range is a boundary point): true when `a` and
/// `b` overlap or abut. Erring toward touch means a click on a hunk stages the whole
/// contiguous change even when the diff split it into adjacent ops.
fn ranges_touch(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

/// Whether any staged block (diff HEAD → index) touches `head_range`. True for a
/// region whose staged version was edited further in the buffer — `staged_flags`
/// reads it unstaged (the working change no longer matches), but an Unstage there
/// would still drop the staged version, so the host offers both actions.
pub fn overlaps_staged(index_hunks: &[Hunk], head_range: (usize, usize)) -> bool {
    index_hunks
        .iter()
        .any(|ih| ranges_touch((ih.old_start, ih.old_start + ih.old_len), head_range))
}

/// One gutter-displayable hunk: a buffer change vs HEAD, or a change that exists
/// only in the index (staged, then reverted/never present in the buffer — e.g. the
/// buffer was restored to HEAD after staging). Staged-only hunks keep the index's
/// new-range so the peek can show the staged content.
pub struct DisplayHunk {
    /// `old` = HEAD line-space, `new` = buffer line-space. For a staged-only hunk
    /// the buffer range holds the (unchanged) HEAD lines the staged change targets.
    pub hunk: Hunk,
    pub staged: bool,
    /// `Some((start, len))` into the INDEX's lines for a staged-only hunk.
    pub index_new: Option<(usize, usize)>,
}

/// Everything the gutter should mark, union of both diffs (Zed's model):
/// diff(base → buffer) hunks with their staged flags, PLUS synthesized staged-only
/// hunks for index changes whose buffer region is back at HEAD — without these, a
/// staged change whose edit was reverted (or never saved) would vanish from the
/// editor while `git diff --cached` still shows it. Sorted by buffer line.
pub fn display_hunks(
    head: Option<&str>,
    index: Option<&str>,
    index_hunks: &[Hunk],
    cur: &str,
) -> Vec<DisplayHunk> {
    let Some(base) = head.or(index) else {
        return Vec::new();
    };
    let cur_hunks = hunks(base, cur);
    let staged = match (head, index) {
        (Some(_), Some(ix)) => staged_flags(ix, index_hunks, cur, &cur_hunks),
        _ => vec![false; cur_hunks.len()],
    };
    let mut out: Vec<DisplayHunk> = cur_hunks
        .iter()
        .zip(&staged)
        .map(|(h, s)| DisplayHunk {
            hunk: *h,
            staged: *s,
            index_new: None,
        })
        .collect();

    if head.is_some() && index.is_some() {
        for ih in index_hunks {
            // Skip index hunks whose HEAD region touches any buffer hunk: either the
            // exact staged change is in the buffer (shown via its staged flag) or the
            // region was re-edited (shown as an unstaged buffer hunk).
            let ih_range = (ih.old_start, ih.old_start + ih.old_len);
            if cur_hunks
                .iter()
                .any(|h| ranges_touch((h.old_start, h.old_start + h.old_len), ih_range))
            {
                continue;
            }
            // Buffer == HEAD across this region, so map HEAD lines to buffer lines by
            // the net line delta of the buffer hunks fully above it.
            let offset: isize = cur_hunks
                .iter()
                .filter(|h| h.old_start + h.old_len <= ih.old_start)
                .map(|h| h.new_len as isize - h.old_len as isize)
                .sum();
            let new_start = (ih.old_start as isize + offset).max(0) as usize;
            out.push(DisplayHunk {
                hunk: Hunk {
                    old_start: ih.old_start,
                    old_len: ih.old_len,
                    new_start,
                    new_len: ih.old_len,
                },
                staged: true,
                index_new: Some((ih.new_start, ih.new_len)),
            });
        }
        out.sort_by_key(|d| (d.hunk.new_start, d.hunk.new_len));
    }
    out
}

/// Replace line ranges of `text` (LF) with new lines, applied bottom-up so earlier
/// coordinates stay valid. `reps` entries are `(start, end_exclusive, new_lines)`.
fn apply_replacements(text: &str, mut reps: Vec<(usize, usize, Vec<String>)>) -> String {
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    reps.sort_by_key(|r| r.0);
    for (start, end, new) in reps.iter().rev() {
        let s = (*start).min(lines.len());
        let e = (*end).min(lines.len()).max(s);
        lines.splice(s..e, new.iter().cloned());
    }
    lines.join("\n")
}

/// The new index content that stages the buffer's change at `buf_range` (a hunk's
/// `[new_start, new_start+new_len]` from the HEAD diff): diff the CURRENT index
/// against the buffer and apply just the touching blocks, so hunks staged earlier
/// are preserved (both diffs share the buffer as their "new" side, so buffer-line
/// ranges are directly comparable). `None` = nothing to stage there.
pub fn stage_apply(index: &str, cur: &str, buf_range: (usize, usize)) -> Option<String> {
    let cl: Vec<&str> = cur.split('\n').collect();
    let mut reps = Vec::new();
    for h in hunks(index, cur) {
        if ranges_touch((h.new_start, h.new_start + h.new_len), buf_range) {
            let s = h.new_start.min(cl.len());
            let e = (h.new_start + h.new_len).min(cl.len()).max(s);
            let new: Vec<String> = cl[s..e].iter().map(|x| x.to_string()).collect();
            reps.push((h.old_start, h.old_start + h.old_len, new));
        }
    }
    if reps.is_empty() {
        return None;
    }
    Some(apply_replacements(index, reps))
}

/// The new index content that UNstages the change at `head_range` (a hunk's
/// `[old_start, old_start+old_len]` from the HEAD diff): diff HEAD against the
/// index and revert the touching staged blocks back to their HEAD lines (both
/// diffs share HEAD as their "old" side). `None` = nothing staged there.
pub fn unstage_apply(head: &str, index: &str, head_range: (usize, usize)) -> Option<String> {
    let hl: Vec<&str> = head.split('\n').collect();
    let mut reps = Vec::new();
    for h in hunks(head, index) {
        if ranges_touch((h.old_start, h.old_start + h.old_len), head_range) {
            let s = h.old_start.min(hl.len());
            let e = (h.old_start + h.old_len).min(hl.len()).max(s);
            let old: Vec<String> = hl[s..e].iter().map(|x| x.to_string()).collect();
            reps.push((h.new_start, h.new_start + h.new_len, old));
        }
    }
    if reps.is_empty() {
        return None;
    }
    Some(apply_replacements(index, reps))
}

/// Discover the repository containing `path` and the repo-relative, forward-slashed
/// path libgit2 keys its index/trees by. Canonicalizes first so `strip_prefix`
/// matches libgit2's canonicalized workdir — else a symlinked path (e.g. macOS
/// `/var` → `/private/var`) would never strip cleanly.
fn repo_rel(path: &Path) -> Option<(git2::Repository, String)> {
    let path = std::fs::canonicalize(path).ok()?;
    let dir = path.parent()?;
    let repo = git2::Repository::discover(dir).ok()?;
    // Canonicalize the workdir too, so both sides share the same form. On Windows
    // `canonicalize` returns a `\\?\C:\…` verbatim path that would never
    // `strip_prefix` libgit2's plain `C:/…` workdir — leaving every file unresolved
    // and the gutter blank. macOS is unaffected (both are already `/…`).
    let workdir = std::fs::canonicalize(repo.workdir()?).ok()?;
    let rel = path.strip_prefix(&workdir).ok()?;
    let rel = rel.to_str()?.replace('\\', "/");
    Some((repo, rel))
}

/// A blob's content, LF-normalized (non-UTF-8 read lossy; code files are UTF-8).
fn blob_text(repo: &git2::Repository, id: git2::Oid) -> Option<String> {
    let blob = repo.find_blob(id).ok()?;
    Some(normalize_newlines(&String::from_utf8_lossy(blob.content())))
}

/// Read the HEAD and index (stage-0) versions of `path`, LF-normalized, as
/// `(head, index)`. Either is `None` when the file isn't there (untracked file, no
/// commit yet, no repo…) — the editor uses HEAD as the diff base (falling back to
/// the index before the first commit) and both together for staged detection.
pub fn git_texts(path: &Path) -> (Option<String>, Option<String>) {
    let Some((repo, rel)) = repo_rel(path) else {
        return (None, None);
    };
    let head = (|| {
        let tree = repo.head().ok()?.peel_to_tree().ok()?;
        let entry = tree.get_path(Path::new(&rel)).ok()?;
        blob_text(&repo, entry.id())
    })();
    let index = (|| {
        let index = repo.index().ok()?;
        let entry = index.get_path(Path::new(&rel), 0)?; // stage 0 = the index
        blob_text(&repo, entry.id)
    })();
    (head, index)
}

/// Read just the index (stage-0) version of `path`, LF-normalized.
#[cfg(test)]
pub fn git_base(path: &Path) -> Option<String> {
    git_texts(path).1
}

/// Write `content_lf` (LF line-space) as the new stage-0 index blob for `path`,
/// preserving the file's line endings (CRLF if the current index blob uses them) and
/// its index entry mode/flags. Returns true on success. This is how a single hunk is
/// staged or unstaged: the caller passes the index with just that hunk applied /
/// reverted (see [`stage_apply`] / [`unstage_apply`]) so only that change moves,
/// like `git add -p`. Non-UTF-8 blobs aren't handled (code files are UTF-8).
pub fn stage_blob(path: &Path, content_lf: &str) -> bool {
    let go = || -> Option<()> {
        let (repo, rel) = repo_rel(path)?;
        let mut index = repo.index().ok()?;
        let entry = index.get_path(Path::new(&rel), 0)?; // reuse mode/path/flags
                                                         // Match the existing blob's line endings so an autocrlf repo isn't rewritten.
        let crlf = repo
            .find_blob(entry.id)
            .ok()
            .map(|b| b.content().windows(2).any(|w| w == b"\r\n"))
            .unwrap_or(false);
        let bytes = if crlf {
            content_lf.replace('\n', "\r\n")
        } else {
            content_lf.to_string()
        };
        index.add_frombuffer(&entry, bytes.as_bytes()).ok()?;
        index.write().ok()?;
        Some(())
    };
    go().is_some()
}

/// Owns the open file's path and drives background git fetches of the HEAD and
/// index texts. The editor requests a fetch on load/focus/save; a later
/// [`poll`](Self::poll) delivers the pair once the worker thread finishes, so the
/// main thread never blocks on git.
pub struct DiffFetcher {
    path: Option<PathBuf>,
    /// Bumped per request; the worker stamps its result with the gen it was for, so
    /// a result from a superseded request (path changed mid-fetch) is discarded.
    gen: u64,
    #[allow(clippy::type_complexity)]
    rx: Option<Receiver<(u64, Option<String>, Option<String>)>>,
}

impl DiffFetcher {
    pub fn new() -> Self {
        Self {
            path: None,
            gen: 0,
            rx: None,
        }
    }

    /// Point at a new file (empty/none clears markers) and kick a fetch.
    pub fn set_path(&mut self, path: Option<PathBuf>) {
        self.path = path;
        self.request();
    }

    /// Re-fetch the texts for the current path (call on focus / after save / branch
    /// change). No-op-ish when there's no path — it just clears on the next poll.
    pub fn request(&mut self) {
        self.gen = self.gen.wrapping_add(1);
        let gen = self.gen;
        let (tx, rx) = std::sync::mpsc::channel();
        self.rx = Some(rx);
        match self.path.clone() {
            Some(path) => {
                // Only a PathBuf + Strings cross the boundary; the repo is opened
                // in-thread, so nothing non-Send escapes.
                std::thread::spawn(move || {
                    let (head, index) = git_texts(&path);
                    let _ = tx.send((gen, head, index));
                });
            }
            None => {
                let _ = tx.send((gen, None, None));
            }
        }
    }

    /// Deliver a finished fetch, if any: `Some((head, index))` for the latest
    /// request (both `None` means "clear markers"). `None` while still pending /
    /// already delivered. Stale results (superseded request) are dropped.
    #[allow(clippy::type_complexity)]
    pub fn poll(&mut self) -> Option<(Option<String>, Option<String>)> {
        let rx = self.rx.as_ref()?;
        match rx.try_recv() {
            Ok((g, head, index)) => {
                self.rx = None;
                if g == self.gen {
                    Some((head, index))
                } else {
                    None
                }
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                None
            }
        }
    }

    /// Write `content_lf` as the new index content for the current path (a stage or
    /// unstage of one hunk), then kick a refresh so the markers pick up the new
    /// index. The index write is fast (one blob) so it runs synchronously; returns
    /// false when there's no path or the write failed.
    pub fn stage(&mut self, content_lf: &str) -> bool {
        let Some(path) = self.path.clone() else {
            return false;
        };
        let ok = stage_blob(&path, content_lf);
        if ok {
            self.request(); // re-read the (now updated) index
        }
        ok
    }
}

impl Default for DiffFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_change_no_markers() {
        assert_eq!(line_markers("a\nb\nc", "a\nb\nc"), vec![0, 0, 0]);
    }

    #[test]
    fn empty_base_is_off() {
        // Untracked / no base: never light up every line (VS Code parity).
        assert_eq!(line_markers("", "a\nb"), vec![0, 0]);
    }

    #[test]
    fn added_lines() {
        // Insert "x" between b and c.
        let m = line_markers("a\nb\nc", "a\nb\nx\nc");
        assert_eq!(m, vec![0, 0, ADDED, 0]);
    }

    #[test]
    fn appended_lines_at_eof() {
        let m = line_markers("a\nb", "a\nb\nc\nd");
        assert_eq!(m, vec![0, 0, ADDED, ADDED]);
    }

    #[test]
    fn modified_line() {
        let m = line_markers("a\nb\nc", "a\nB\nc");
        assert_eq!(m, vec![0, MODIFIED, 0]);
    }

    #[test]
    fn deleted_interior_marks_boundary() {
        // Remove "b": the gap sits above the line that was "c" (now index 1).
        let m = line_markers("a\nb\nc", "a\nc");
        assert_eq!(m, vec![0, DELETED_ABOVE]);
    }

    #[test]
    fn deleted_at_eof_marks_below_last() {
        let m = line_markers("a\nb\nc", "a\nb");
        assert_eq!(m, vec![0, DELETED_BELOW]);
    }

    #[test]
    fn crlf_base_matches_lf_buffer() {
        // A CRLF index blob normalized to LF must not read as all-modified.
        let base = normalize_newlines("a\r\nb\r\nc");
        assert_eq!(line_markers(&base, "a\nb\nc"), vec![0, 0, 0]);
    }

    #[test]
    fn hunks_expose_old_range_for_peek() {
        // Modified line: the old line is recoverable for a peek.
        let base = "a\nb\nc";
        let cur = "a\nB\nc";
        let hs = hunks(base, cur);
        assert_eq!(
            hs,
            vec![Hunk {
                old_start: 1,
                old_len: 1,
                new_start: 1,
                new_len: 1
            }]
        );
        let old: Vec<&str> = base.split('\n').collect();
        assert_eq!(
            &old[hs[0].old_start..hs[0].old_start + hs[0].old_len],
            &["b"]
        );

        // Pure deletion: new_len 0, boundary at the line the removed text sat above.
        let hs = hunks("a\nb\nc", "a\nc");
        assert_eq!(
            hs,
            vec![Hunk {
                old_start: 1,
                old_len: 1,
                new_start: 1,
                new_len: 0
            }]
        );

        // Pure addition: nothing removed (old_len 0), so no peek content.
        let hs = hunks("a\nc", "a\nb\nc");
        assert_eq!(
            hs,
            vec![Hunk {
                old_start: 1,
                old_len: 0,
                new_start: 1,
                new_len: 1
            }]
        );
    }

    #[test]
    fn markers_match_the_direct_path() {
        // markers_from_hunks(hunks(..)) is what line_markers is built on.
        for (b, c) in [
            ("a\nb\nc", "a\nB\nc"),
            ("a\nb", "a\nb\nc\nd"),
            ("a\nb\nc", "a\nb"),
        ] {
            assert_eq!(
                line_markers(b, c),
                markers_from_hunks(&hunks(b, c), c.split('\n').count())
            );
        }
    }

    #[test]
    fn stage_apply_stages_one_hunk_and_keeps_earlier_staged() {
        // HEAD: a b c d e. Buffer changes b→B and d→D (two hunks vs HEAD).
        let head = "a\nb\nc\nd\ne";
        let cur = "a\nB\nc\nD\ne";
        let hs = hunks(head, cur);
        assert_eq!(hs.len(), 2);

        // Stage the first hunk: index starts equal to HEAD.
        let idx1 = stage_apply(
            head,
            cur,
            (hs[0].new_start, hs[0].new_start + hs[0].new_len),
        )
        .unwrap();
        assert_eq!(idx1, "a\nB\nc\nd\ne", "only the b→B hunk staged");

        // Stage the second hunk against the UPDATED index: B must be preserved
        // (this is the clobber the index-space diff avoids).
        let idx2 = stage_apply(
            &idx1,
            cur,
            (hs[1].new_start, hs[1].new_start + hs[1].new_len),
        )
        .unwrap();
        assert_eq!(
            idx2, "a\nB\nc\nD\ne",
            "second stage keeps the earlier staged hunk"
        );

        // Pure deletion: buffer removed "b"; staging drops it from the index.
        let head = "a\nb\nc";
        let cur = "a\nc";
        let hd = hunks(head, cur);
        let idx = stage_apply(
            head,
            cur,
            (hd[0].new_start, hd[0].new_start + hd[0].new_len),
        )
        .unwrap();
        assert_eq!(idx, "a\nc");

        // Nothing to stage (buffer == index) → None.
        assert_eq!(stage_apply("a\nb", "a\nb", (0, 1)), None);
    }

    #[test]
    fn unstage_apply_reverts_one_staged_hunk() {
        // HEAD: a b c d e. Index has both hunks staged; unstage only the first.
        let head = "a\nb\nc\nd\ne";
        let index = "a\nB\nc\nD\ne";
        let sh = hunks(head, index);
        assert_eq!(sh.len(), 2);
        let idx = unstage_apply(
            head,
            index,
            (sh[0].old_start, sh[0].old_start + sh[0].old_len),
        )
        .unwrap();
        assert_eq!(idx, "a\nb\nc\nD\ne", "only the first hunk reverted to HEAD");

        // Unstaging a staged deletion restores the removed line.
        let idx = unstage_apply("a\nb\nc", "a\nc", (1, 2)).unwrap();
        assert_eq!(idx, "a\nb\nc");

        // Nothing staged there → None.
        assert_eq!(unstage_apply("a\nb", "a\nb", (0, 1)), None);
    }

    #[test]
    fn staged_flags_match_exact_index_change() {
        let head = "a\nb\nc\nd\ne";
        let cur = "a\nB\nc\nD\ne";
        let cur_hunks = hunks(head, cur);

        // Index has only the b→B hunk staged.
        let index = "a\nB\nc\nd\ne";
        let index_hunks = hunks(head, index);
        let flags = staged_flags(index, &index_hunks, cur, &cur_hunks);
        assert_eq!(flags, vec![true, false]);

        // Editing the staged region further makes it read unstaged again.
        let cur2 = "a\nBB\nc\nD\ne";
        let cur2_hunks = hunks(head, cur2);
        let flags2 = staged_flags(index, &index_hunks, cur2, &cur2_hunks);
        assert_eq!(flags2, vec![false, false]);

        // A staged pure deletion matches by ranges (no content to compare).
        let head = "a\nb\nc";
        let cur = "a\nc";
        let index = "a\nc";
        let flags3 = staged_flags(index, &hunks(head, index), cur, &hunks(head, cur));
        assert_eq!(flags3, vec![true]);
    }

    #[test]
    fn display_hunks_synthesizes_staged_only() {
        // Staged b→B, but the buffer is back at HEAD: the editor must still show a
        // staged hunk there (matching `git diff --cached`), peeking the index lines.
        let head = "a\nb\nc";
        let index = "a\nB\nc";
        let ih = hunks(head, index);
        let cur = head; // buffer == HEAD
        let ds = display_hunks(Some(head), Some(index), &ih, cur);
        assert_eq!(ds.len(), 1);
        let d = &ds[0];
        assert!(d.staged);
        assert_eq!(d.index_new, Some((1, 1)));
        assert_eq!(
            d.hunk,
            Hunk {
                old_start: 1,
                old_len: 1,
                new_start: 1,
                new_len: 1
            }
        );

        // Unstaging it from the synthesized hunk's HEAD range restores the index.
        let restored = unstage_apply(
            head,
            index,
            (d.hunk.old_start, d.hunk.old_start + d.hunk.old_len),
        )
        .unwrap();
        assert_eq!(restored, head);
    }

    #[test]
    fn display_hunks_maps_staged_only_past_buffer_edits() {
        // A buffer insertion ABOVE the staged-only region shifts its buffer lines.
        let head = "a\nb\nc\nd";
        let index = "a\nb\nC\nd"; // staged c→C
        let ih = hunks(head, index);
        let cur = "X\na\nb\nc\nd"; // unsaved insertion at top; c is now buffer line 3
        let ds = display_hunks(Some(head), Some(index), &ih, cur);
        let so: Vec<_> = ds.iter().filter(|d| d.index_new.is_some()).collect();
        assert_eq!(so.len(), 1);
        assert_eq!(
            so[0].hunk.new_start, 3,
            "staged-only hunk mapped past the insertion"
        );

        // And the ordinary insertion hunk is still there, unstaged.
        assert!(ds.iter().any(|d| d.index_new.is_none() && !d.staged));
    }

    #[test]
    fn display_hunks_skips_staged_only_when_region_reedited() {
        // The staged region was edited again in the buffer: show ONE unstaged buffer
        // hunk there, not a duplicate staged-only hunk.
        let head = "a\nb\nc";
        let index = "a\nB\nc"; // staged b→B
        let ih = hunks(head, index);
        let cur = "a\nBB\nc"; // buffer re-edited the same line
        let ds = display_hunks(Some(head), Some(index), &ih, cur);
        assert_eq!(ds.len(), 1);
        assert!(ds[0].index_new.is_none());
        assert!(!ds[0].staged);
    }

    #[test]
    fn editing_a_staged_hunk_reads_unstaged_then_restages() {
        // Stage b→B, then edit the same line further in the buffer (B→BB, unsaved).
        let head = "a\nb\nc";
        let index = "a\nB\nc"; // the staged version
        let ih = hunks(head, index);
        let cur = "a\nBB\nc"; // edited beyond what's staged

        // 1. The hunk reads UNSTAGED again (solid marker), shown once (no duplicate
        //    staged-only hunk), and the index is untouched by the edit.
        let ds = display_hunks(Some(head), Some(index), &ih, cur);
        assert_eq!(ds.len(), 1);
        assert!(
            !ds[0].staged,
            "further-edited staged hunk must read unstaged"
        );
        assert!(ds[0].index_new.is_none(), "no duplicate staged-only hunk");

        // 2. Re-staging replaces the old staged version with the current content.
        let h = ds[0].hunk;
        let restaged = stage_apply(index, cur, (h.new_start, h.new_start + h.new_len)).unwrap();
        assert_eq!(restaged, "a\nBB\nc");

        // 3. The region still reads "has staged content" (partially staged), so the
        //    menu offers Unstage alongside Stage — and unstage_apply drops the old
        //    staged version wholesale.
        assert!(overlaps_staged(&ih, (h.old_start, h.old_start + h.old_len)));
        assert!(
            !overlaps_staged(&ih, (10, 12)),
            "untouched region has nothing staged"
        );
        let dropped = unstage_apply(head, index, (h.old_start, h.old_start + h.old_len)).unwrap();
        assert_eq!(dropped, head);
    }

    #[test]
    fn apply_staged_bits_marks_lines_and_boundaries() {
        let head = "a\nb\nc\nd";
        let cur = "a\nB\nd"; // b→B (staged), c deleted (unstaged)
        let hs = hunks(head, cur);
        let n = cur.split('\n').count();
        let mut marks = markers_from_hunks(&hs, n);
        // hunks: one Replace (b,c → B)? Depends on Myers; compute flags directly.
        let staged: Vec<bool> = hs.iter().map(|h| h.new_len > 0).collect();
        apply_staged_bits(&mut marks, &hs, &staged);
        for (i, m) in marks.iter().enumerate() {
            if m & (ADDED | MODIFIED) != 0 {
                assert_ne!(m & STAGED, 0, "line {i} bar should carry STAGED");
            }
        }
    }

    #[test]
    fn stage_blob_shows_staged_vs_head() {
        // A repo WITH a commit (like a real project): staging a hunk must show up as
        // an index-vs-HEAD change, which is what `git status` / `git diff --cached` read.
        let dir = std::env::temp_dir().join(format!("unterm-stagehead-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = git2::Repository::init(&dir).unwrap();
        let file = dir.join("foo.txt");
        std::fs::write(&file, "a\nb\nc\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("foo.txt")).unwrap();
        index.write().unwrap();
        // commit so HEAD exists
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        assert!(stage_blob(&file, "a\nB\nc\n"), "stage_blob returned false");

        // index (stage-0) must now differ from HEAD for foo.txt. Use a FRESH repo
        // handle: the original one caches its index in memory and wouldn't see the
        // on-disk write stage_blob made through its own handle.
        let repo = git2::Repository::discover(&dir).unwrap();
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let fresh = repo.index().unwrap();
        let diff = repo
            .diff_tree_to_index(Some(&head_tree), Some(&fresh), None)
            .unwrap();
        let changed: Vec<_> = diff
            .deltas()
            .map(|d| d.new_file().path().unwrap().to_path_buf())
            .collect();
        assert!(
            changed.iter().any(|p| p.ends_with("foo.txt")),
            "staged change not visible vs HEAD; deltas = {changed:?}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stage_blob_updates_index_and_keeps_crlf() {
        let dir = std::env::temp_dir().join(format!("unterm-stage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = git2::Repository::init(&dir).unwrap();
        let file = dir.join("foo.txt");
        std::fs::write(&file, "a\r\nb\r\nc\r\n").unwrap(); // CRLF on disk
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("foo.txt")).unwrap();
        index.write().unwrap();

        // Stage new LF content — stage_blob must re-apply CRLF to match the blob.
        assert!(stage_blob(&file, "a\nB\nc\n"));
        assert_eq!(git_base(&file).as_deref(), Some("a\nB\nc\n")); // LF-normalized read

        let repo2 = git2::Repository::discover(&dir).unwrap();
        let entry = repo2
            .index()
            .unwrap()
            .get_path(Path::new("foo.txt"), 0)
            .unwrap();
        let raw = repo2.find_blob(entry.id).unwrap().content().to_vec();
        assert_eq!(
            raw, b"a\r\nB\r\nc\r\n",
            "line endings preserved in the index blob"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn git_texts_reads_head_and_index() {
        let dir = std::env::temp_dir().join(format!("unterm-texts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = git2::Repository::init(&dir).unwrap();
        let file = dir.join("foo.txt");

        // Staged but no commit yet: head None, index Some.
        std::fs::write(&file, "one\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("foo.txt")).unwrap();
        index.write().unwrap();
        assert_eq!(git_texts(&file), (None, Some("one\n".into())));

        // Commit, then stage a different version: head and index diverge.
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        assert!(stage_blob(&file, "two\n"));
        assert_eq!(
            git_texts(&file),
            (Some("one\n".into()), Some("two\n".into()))
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn git_base_reads_staged_blob() {
        // End-to-end through git2: a real temp repo with a staged file. The base we
        // read back must be the staged content (LF-normalized), and an untracked
        // sibling must yield None.
        let dir = std::env::temp_dir().join(format!("unterm-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = git2::Repository::init(&dir).unwrap();

        let tracked = dir.join("foo.txt");
        std::fs::write(&tracked, "one\r\ntwo\r\nthree\r\n").unwrap(); // CRLF on disk
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("foo.txt")).unwrap();
        index.write().unwrap();

        assert_eq!(git_base(&tracked).as_deref(), Some("one\ntwo\nthree\n"));

        let untracked = dir.join("bar.txt");
        std::fs::write(&untracked, "x").unwrap();
        assert_eq!(git_base(&untracked), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
