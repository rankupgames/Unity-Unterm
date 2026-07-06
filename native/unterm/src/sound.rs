//! Play the bundled "agent done" chime when a turn finishes while the window is
//! in the background. The WAV is embedded in the plugin (no asset path to
//! resolve, survives being loaded from anywhere), and played by the OS: `NSSound`
//! on macOS, `PlaySound` on Windows. Best-effort — a failure is silent, never a
//! panic across the FFI boundary (the caller still wraps it in `ffi_guard`).
#![cfg(any(target_os = "macos", windows))]

/// The chime (48 kHz/16-bit stereo PCM WAV). Kept in the version-controlled
/// `native/` tree and compiled into the binary.
static AGENT_DONE_WAV: &[u8] = include_bytes!("../assets/agent_done.wav");

#[cfg(target_os = "macos")]
mod imp {
    use super::AGENT_DONE_WAV;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use std::cell::RefCell;
    use std::ffi::c_void;

    thread_local! {
        // Hold the currently-playing sound alive: `-play` is asynchronous, so a
        // dropped (released) `NSSound` would cut the chime short. Exactly one is
        // kept — a fresh chime replaces (and stops) any still ringing.
        static CURRENT: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    }

    pub fn play_agent_done() {
        // AppKit is main-thread only; the host calls this from the editor update.
        unsafe {
            let data: *mut AnyObject = msg_send![
                class!(NSData),
                dataWithBytes: AGENT_DONE_WAV.as_ptr() as *const c_void,
                length: AGENT_DONE_WAV.len(),
            ];
            if data.is_null() {
                return;
            }
            let alloc: *mut AnyObject = msg_send![class!(NSSound), alloc];
            // `initWithData:` returns a +1 reference we take ownership of below.
            let sound: *mut AnyObject = msg_send![alloc, initWithData: data];
            let Some(sound) = Retained::from_raw(sound) else {
                return;
            };
            let _: bool = msg_send![&*sound, play];
            CURRENT.with(|c| *c.borrow_mut() = Some(sound));
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::AGENT_DONE_WAV;
    use windows::core::PCWSTR;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

    pub fn play_agent_done() {
        // Under SND_MEMORY the "name" pointer is the in-memory WAV image; async so
        // it doesn't block the editor tick, NODEFAULT so a bad image is silent
        // rather than the system default beep. `hmod` is unused with SND_MEMORY.
        unsafe {
            let _ = PlaySoundW(
                PCWSTR(AGENT_DONE_WAV.as_ptr() as *const u16),
                None,
                SND_ASYNC | SND_MEMORY | SND_NODEFAULT,
            );
        }
    }
}

pub use imp::play_agent_done;
