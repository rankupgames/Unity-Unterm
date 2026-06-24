using System;
using System.IO;
using System.Runtime.InteropServices;
using UnityEngine;

namespace Unterm.Editor
{
    /// <summary>
    /// Dynamic-library loader for the Unterm native terminal plugin (macOS +
    /// Windows).
    ///
    /// Terminals live in process globals on the native side (the registry keyed
    /// by a stable u64 id), so they survive Unity C# domain reloads. To keep
    /// them mapped we load the library via a *stable* shadow copy and never
    /// unload it on reload. Every editor window loads the same shadow path, so
    /// they all share one native image and one registry; each window owns one
    /// terminal id it serializes and re-adopts after a reload.
    ///
    /// The OS dynamic loader is used directly (dlopen on macOS, LoadLibrary on
    /// Windows) rather than Unity's native-plugin import system, so we control
    /// when the image loads/unloads across reloads.
    /// </summary>
    internal sealed class UntermNative : IDisposable
    {
        // --- platform dynamic-loader shim -------------------------------------
#if UNITY_EDITOR_WIN
        [DllImport("kernel32", SetLastError = true, CharSet = CharSet.Unicode)]
        private static extern IntPtr LoadLibrary(string path);
        // GetProcAddress takes an ANSI symbol name regardless of the wide module API.
        [DllImport("kernel32", SetLastError = true)]
        private static extern IntPtr GetProcAddress(IntPtr handle, [MarshalAs(UnmanagedType.LPStr)] string symbol);
        [DllImport("kernel32", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool FreeLibrary(IntPtr handle);

        private static IntPtr NativeOpen(string path) => LoadLibrary(path);
        private static IntPtr NativeSym(IntPtr handle, string symbol) => GetProcAddress(handle, symbol);
        private static void NativeClose(IntPtr handle) => FreeLibrary(handle);
        private static string NativeError() =>
            new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error()).Message;
#else
        // Shadow copies keep a .dylib extension on macOS.
        private const string ShadowExt = ".dylib";

        private const int RTLD_NOW = 2;
        private const int RTLD_LOCAL = 4;

        [DllImport("/usr/lib/libSystem.B.dylib")]
        private static extern IntPtr dlopen(string path, int mode);
        [DllImport("/usr/lib/libSystem.B.dylib")]
        private static extern IntPtr dlsym(IntPtr handle, string symbol);
        [DllImport("/usr/lib/libSystem.B.dylib")]
        private static extern int dlclose(IntPtr handle);
        [DllImport("/usr/lib/libSystem.B.dylib")]
        private static extern IntPtr dlerror();

        private static IntPtr NativeOpen(string path) => dlopen(path, RTLD_NOW | RTLD_LOCAL);
        private static IntPtr NativeSym(IntPtr handle, string symbol) => dlsym(handle, symbol);
        private static void NativeClose(IntPtr handle) => dlclose(handle);
        private static string NativeError()
        {
            var p = dlerror();
            return p == IntPtr.Zero ? "(no error)" : Marshal.PtrToStringAnsi(p);
        }
#endif

        // --- terminal registry C ABI (id-based; survives reload) ---
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate ulong CreateSeededFn(ulong id, uint w, uint h, float scale, [MarshalAs(UnmanagedType.LPUTF8Str)] string cwd, [MarshalAs(UnmanagedType.LPUTF8Str)] string seed);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate ulong CreateDeadFn(ulong id, uint w, uint h, float scale, [MarshalAs(UnmanagedType.LPUTF8Str)] string seed);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate ulong CreateFn(uint w, uint h, float scale, [MarshalAs(UnmanagedType.LPUTF8Str)] string cwd);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate ulong CreateCommandFn(uint w, uint h, float scale, [MarshalAs(UnmanagedType.LPUTF8Str)] string cwd, [MarshalAs(UnmanagedType.LPUTF8Str)] string command);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] [return: MarshalAs(UnmanagedType.I1)] private delegate bool ExistsFn(ulong id);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void IdFn(ulong id);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void ResizeFn(ulong id, uint w, uint h, float scale);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void SetScaleFn(ulong id, float scale);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void SetFontFn(ulong id, [MarshalAs(UnmanagedType.LPUTF8Str)] string path);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void SetFontSizeFn(ulong id, float points);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void SetColorsFn(ulong id, uint fg, uint bg, uint cursor);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void SetFocusFn(ulong id, [MarshalAs(UnmanagedType.I1)] bool focused);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void SendTextFn(ulong id, [MarshalAs(UnmanagedType.LPUTF8Str)] string text);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void SendKeyFn(ulong id, [MarshalAs(UnmanagedType.LPUTF8Str)] string name, [MarshalAs(UnmanagedType.I1)] bool ctrl, [MarshalAs(UnmanagedType.I1)] bool alt, [MarshalAs(UnmanagedType.I1)] bool shift);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void ScrollFn(ulong id, int delta);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void SelStartFn(ulong id, float x, float y, byte mode);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void SelUpdateFn(ulong id, float x, float y);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] [return: MarshalAs(UnmanagedType.I1)] private delegate bool BoolFn(ulong id);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate IntPtr PtrFn(ulong id);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void SizeFn(ulong id, out uint a, out uint b);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void ScrollStateFn(ulong id, out uint history, out uint offset, out uint screen);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] [return: MarshalAs(UnmanagedType.I1)] private delegate bool CursorPxFn(ulong id, out float x, out float y, out float w, out float h);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate IntPtr TitleFn(ulong id, out UIntPtr len);

        private IntPtr _handle;
        private string _shadowPath;
        private bool _stable;

        private CreateFn _create; private CreateCommandFn _createCommand; private ExistsFn _exists; private IdFn _destroy; private ResizeFn _resize;
        // Restore-across-restart: seed+shell, seed-only (display-only/exited), the buffer dump, the cwd.
        private CreateSeededFn _createSeeded; private CreateDeadFn _createDead; private TitleFn _dump; private TitleFn _cwd;
        private SetScaleFn _setScale; private SetFontFn _setFont; private SetFontSizeFn _setFontSize;
        private SetColorsFn _setColors; private SetFocusFn _setFocus; private SendTextFn _sendText;
        private SendTextFn _setPreedit;
        private SendKeyFn _sendKey; private SendTextFn _paste; private IdFn _clear;
        private ScrollFn _scroll; private IdFn _render; private BoolFn _dirty; private BoolFn _present;
        private SelStartFn _selStart; private SelUpdateFn _selUpdate; private IdFn _selClear; private TitleFn _selText;
        private BoolFn _isAlive; private PtrFn _iosurface; private PtrFn _rawTexture;
        private SizeFn _size; private SizeFn _gridSize; private ScrollStateFn _scrollState; private CursorPxFn _cursorPx; private TitleFn _title;

        public bool IsLoaded => _handle != IntPtr.Zero;

        /// <summary>
        /// Load the bundle via a shadow copy. With <paramref name="freshInstance"/>
        /// false (default) a stable shadow path keyed on the bundle's identity is
        /// reused, so re-loading after a domain reload returns the same mapped
        /// image (the terminal registry persists). Pass true to force a brand-new
        /// image (picks up a rebuilt Rust bundle).
        /// </summary>
        public void Load(string bundlePath, bool freshInstance = false)
        {
            if (IsLoaded) return;
#if UNITY_EDITOR_WIN
            // Bind to the SAME image Unity already loaded as an Editor/Windows
            // native plugin: a bare-name LoadLibrary resolves to the in-process
            // module by base name, so UnityPluginLoad — which captured the editor's
            // D3D device — ran in this very image, and the zero-copy surface uses
            // that device directly (no shadow copy, no cross-image device bridge).
            // Unity keeps editor plugins mapped across domain reloads, so the
            // terminal registry survives without our own shadow-copy trick.
            _stable = true; // not a temp file we own, so never delete on Dispose
            _handle = NativeOpen("unterm");
            if (_handle == IntPtr.Zero)
                throw new Exception(
                    $"native load failed — is unterm.dll imported as an Editor/Windows plugin? {NativeError()}");
#else
            if (!File.Exists(bundlePath))
                throw new FileNotFoundException($"Unterm native bundle not found: {bundlePath}");

            _stable = !freshInstance;
            var info = new FileInfo(bundlePath);
            _shadowPath = freshInstance
                ? Path.Combine(Path.GetTempPath(), $"unterm_{Guid.NewGuid():N}{ShadowExt}")
                : Path.Combine(Path.GetTempPath(), $"unterm_{info.Length}_{info.LastWriteTimeUtc.Ticks}{ShadowExt}");

            if (freshInstance || !File.Exists(_shadowPath))
                File.Copy(bundlePath, _shadowPath, overwrite: freshInstance);

            _handle = NativeOpen(_shadowPath);
            if (_handle == IntPtr.Zero)
                throw new Exception($"native load failed: {NativeError()}");
#endif

            _create = Sym<CreateFn>("unterm_create");
            _createCommand = Sym<CreateCommandFn>("unterm_create_command");
            _createSeeded = Sym<CreateSeededFn>("unterm_create_seeded");
            _createDead = Sym<CreateDeadFn>("unterm_create_dead");
            _dump = Sym<TitleFn>("unterm_dump");
            _cwd = Sym<TitleFn>("unterm_cwd");
            _exists = Sym<ExistsFn>("unterm_exists");
            _destroy = Sym<IdFn>("unterm_destroy");
            _resize = Sym<ResizeFn>("unterm_resize");
            _setScale = Sym<SetScaleFn>("unterm_set_scale");
            _setFont = Sym<SetFontFn>("unterm_set_font");
            _setFontSize = Sym<SetFontSizeFn>("unterm_set_font_size");
            _setColors = Sym<SetColorsFn>("unterm_set_colors");
            _setFocus = Sym<SetFocusFn>("unterm_set_focus");
            _sendText = Sym<SendTextFn>("unterm_send_text");
            _setPreedit = Sym<SendTextFn>("unterm_set_preedit");
            _sendKey = Sym<SendKeyFn>("unterm_send_key");
            _paste = Sym<SendTextFn>("unterm_paste");
            _clear = Sym<IdFn>("unterm_clear");
            _scroll = Sym<ScrollFn>("unterm_scroll");
            _selStart = Sym<SelStartFn>("unterm_selection_start");
            _selUpdate = Sym<SelUpdateFn>("unterm_selection_update");
            _selClear = Sym<IdFn>("unterm_selection_clear");
            _selText = Sym<TitleFn>("unterm_selection_text");
            _render = Sym<IdFn>("unterm_render");
            _dirty = Sym<BoolFn>("unterm_dirty");
            _present = Sym<BoolFn>("unterm_present");
            _isAlive = Sym<BoolFn>("unterm_is_alive");
            _iosurface = Sym<PtrFn>("unterm_iosurface");
            _rawTexture = Sym<PtrFn>("unterm_raw_texture");
            _size = Sym<SizeFn>("unterm_size");
            _gridSize = Sym<SizeFn>("unterm_grid_size");
            _scrollState = Sym<ScrollStateFn>("unterm_scroll_state");
            _cursorPx = Sym<CursorPxFn>("unterm_cursor_px");
            _title = Sym<TitleFn>("unterm_title");
        }

        private T Sym<T>(string name) where T : Delegate
        {
            var addr = NativeSym(_handle, name);
            if (addr == IntPtr.Zero)
                throw new Exception($"symbol '{name}' not found: {NativeError()}");
            return Marshal.GetDelegateForFunctionPointer<T>(addr);
        }

        private static string Utf8(IntPtr p, UIntPtr len) =>
            p == IntPtr.Zero ? string.Empty : Marshal.PtrToStringUTF8(p, (int)len.ToUInt64());

        public ulong Create(uint w, uint h, float scale, string cwd) => _create(w, h, scale, cwd ?? string.Empty);
        /// Create a terminal that launches `command` directly in the PTY (no shell prompt / typed input).
        public ulong CreateCommand(uint w, uint h, float scale, string cwd, string command) =>
            _createCommand(w, h, scale, cwd ?? string.Empty, command ?? string.Empty);
        /// Restore an interactive shell with the grid pre-seeded; re-claims terminal id `id` if free.
        public ulong CreateSeeded(ulong id, uint w, uint h, float scale, string cwd, string seed) =>
            _createSeeded(id, w, h, scale, cwd ?? string.Empty, seed ?? string.Empty);
        /// Restore a display-only terminal (no shell, marked exited); re-claims terminal id `id` if free.
        public ulong CreateDead(ulong id, uint w, uint h, float scale, string seed) =>
            _createDead(id, w, h, scale, seed ?? string.Empty);
        /// The full buffer (scrollback + screen) as truecolor-SGR text, for saving across a restart.
        public string Dump(ulong id)
        {
            var p = _dump(id, out UIntPtr len);
            return Utf8(p, len);
        }
        /// The shell's current working directory (empty if no live shell), for restoring cwd on resume.
        public string Cwd(ulong id)
        {
            var p = _cwd(id, out UIntPtr len);
            return Utf8(p, len);
        }
        public bool Exists(ulong id) => id != 0 && _exists(id);
        public void Destroy(ulong id) { if (id != 0) _destroy(id); }
        public void Resize(ulong id, uint w, uint h, float scale) => _resize(id, w, h, scale);
        public void SetScale(ulong id, float scale) => _setScale(id, scale);
        public void SetFont(ulong id, string path) => _setFont(id, path ?? string.Empty);
        public void SetFontSize(ulong id, float points) => _setFontSize(id, points);
        public void SetColors(ulong id, Color32 fg, Color32 bg, Color32 cursor) =>
            _setColors(id, Pack(fg), Pack(bg), Pack(cursor));
        public void SetFocus(ulong id, bool focused) => _setFocus(id, focused);
        public void SendText(ulong id, string text) { if (!string.IsNullOrEmpty(text)) _sendText(id, text); }
        // Empty string clears the composition, so always forward (don't short-circuit).
        public void SetPreedit(ulong id, string text) => _setPreedit(id, text ?? "");
        public void SendKey(ulong id, string name, bool ctrl, bool alt, bool shift) => _sendKey(id, name, ctrl, alt, shift);
        public void Paste(ulong id, string text) { if (!string.IsNullOrEmpty(text)) _paste(id, text); }
        public void Clear(ulong id) => _clear(id);
        public void Scroll(ulong id, int delta) => _scroll(id, delta);
        // mode: 0 = by character, 1 = by word (double-click), 2 = by line.
        public void SelectionStart(ulong id, float x, float y, byte mode) => _selStart(id, x, y, mode);
        public void SelectionUpdate(ulong id, float x, float y) => _selUpdate(id, x, y);
        public void SelectionClear(ulong id) => _selClear(id);
        public string SelectionText(ulong id)
        {
            var p = _selText(id, out UIntPtr len);
            return Utf8(p, len);
        }
        public void Render(ulong id) => _render(id);
        public bool Dirty(ulong id) => _dirty(id);
        // Advance the render-target swapchain; true if the displayed frame changed.
        public bool Present(ulong id) => _present(id);
        public bool IsAlive(ulong id) => _isAlive(id);
        public IntPtr IOSurface(ulong id) => _iosurface(id);
        public IntPtr RawTexture(ulong id) => _rawTexture(id);
        public void Size(ulong id, out uint w, out uint h) => _size(id, out w, out h);
        public void GridSize(ulong id, out uint cols, out uint rows) => _gridSize(id, out cols, out rows);
        // history = scrollback lines above the screen; offset = lines scrolled up
        // from the live bottom (0 = pinned); screen = visible row count.
        public void ScrollState(ulong id, out uint history, out uint offset, out uint screen) =>
            _scrollState(id, out history, out offset, out screen);
        public bool CursorPx(ulong id, out float x, out float y, out float w, out float h) =>
            _cursorPx(id, out x, out y, out w, out h);
        public string Title(ulong id)
        {
            var p = _title(id, out UIntPtr len);
            return Utf8(p, len);
        }

        private static uint Pack(Color32 c) => (uint)((c.r << 16) | (c.g << 8) | c.b);

        public void Dispose()
        {
            if (_handle != IntPtr.Zero)
            {
                // Best-effort: the OS keeps the image mapped while other (leaked)
                // refs from prior reloads or sibling windows remain — intended,
                // the native globals must outlive any single managed wrapper.
                NativeClose(_handle);
                _handle = IntPtr.Zero;
            }
            _create = null; _createCommand = null; _createSeeded = null; _createDead = null; _dump = null; _cwd = null;
            _exists = null; _destroy = null; _resize = null; _setScale = null;
            _setFont = null; _setFontSize = null; _setColors = null; _setFocus = null;
            _sendText = null; _sendKey = null; _scroll = null; _render = null; _dirty = null; _present = null;
            _selStart = null; _selUpdate = null; _selClear = null; _selText = null;
            _isAlive = null; _iosurface = null; _rawTexture = null;
            _paste = null; _clear = null;
            _size = null; _gridSize = null; _scrollState = null; _cursorPx = null; _title = null;

            if (!_stable && !string.IsNullOrEmpty(_shadowPath) && File.Exists(_shadowPath))
            {
                try { File.Delete(_shadowPath); } catch { /* best effort */ }
            }
            _shadowPath = null;
        }
    }
}
