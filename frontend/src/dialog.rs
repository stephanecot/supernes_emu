//! Native file/folder dialogs, opened off the winit callback stack.
//!
//! **Why this module exists.** winit 0.30.13's macOS backend deliberately
//! panics on re-entrancy: `platform_impl/macos/event_handler.rs` keeps the
//! application handler borrowed for the whole duration of a callback and
//! `handle_event` ends with `panic!("tried to handle event while another event
//! is currently being handled")` when it is entered again. A native modal
//! (`NSOpenPanel`/`NSAlert` `runModal`) spins a *nested* AppKit run loop, which
//! fires winit's run-loop observers while our callback is still on the stack —
//! the panic then unwinds through Objective-C frames and aborts the process.
//! That is the crash documented in `docs/PUNCHLIST.md`, and it applies to every
//! `ApplicationHandler` method, `about_to_wait` included (winit dispatches
//! `Event::AboutToWait` through the very same `handle_event`), so simply
//! deferring the call to the next iteration is *not* a fix.
//!
//! **What this module does.** A request is queued, then posted to libdispatch's
//! main queue: the run loop drains that queue between winit callbacks, i.e. on a
//! stack where no handler is borrowed, so the nested modal loop is legal. The
//! answer comes back over a channel the event loop polls on a later frame.
//! Platforms other than macOS have no such guard and no nested-loop problem, so
//! the request simply runs inline when the event loop pumps it.
//!
//! Every call into `picker` (and therefore into `rfd`) made while the event loop
//! is running goes through here — `video.rs` only ever queues a `Request`, which
//! a unit test below pins.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

/// A native dialog the shell wants to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Pick a cartridge to load; `start` is the folder the panel opens in.
    Rom { start: PathBuf },
    /// Pick the folder the library scans.
    LibraryDir { current: PathBuf },
    /// Pick the folder screenshots are written to.
    ScreenshotDir { current: PathBuf },
    /// Pick the folder battery saves and save states are written to.
    SaveDir { current: PathBuf },
}

/// What the player answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Rom(PathBuf),
    LibraryDir(PathBuf),
    ScreenshotDir(PathBuf),
    SaveDir(PathBuf),
    /// The panel was dismissed without a choice; the caller changes nothing but
    /// still restarts frame pacing, since an arbitrary amount of wall time
    /// passed.
    Cancelled,
}

/// Pending native dialog, if any. One at a time: two native panels at once is
/// not a state the platform offers, so a request made while one is on screen is
/// dropped rather than stacked.
pub struct Dialogs {
    queued: Option<Request>,
    on_screen: bool,
    tx: Sender<Answer>,
    rx: Receiver<Answer>,
}

impl Default for Dialogs {
    fn default() -> Self {
        Self::new()
    }
}

impl Dialogs {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self { queued: None, on_screen: false, tx, rx }
    }

    /// Queue `request`; ignored while another dialog is queued or on screen.
    pub fn request(&mut self, request: Request) {
        if self.is_busy() {
            return;
        }
        self.queued = Some(request);
    }

    /// True while a dialog is queued or waiting for an answer. The event loop
    /// suspends emulation in the meantime: the panel owns the keyboard, and the
    /// player is not looking at the game.
    pub fn is_busy(&self) -> bool {
        self.on_screen || self.queued.is_some()
    }

    /// Open the queued dialog, if any. Must be called from the event loop's own
    /// iteration (`about_to_wait`), never from a place that then keeps running
    /// UI code — on macOS this only *posts* the work, so it returns immediately.
    pub fn pump(&mut self) {
        if self.on_screen {
            return;
        }
        let Some(request) = self.queued.take() else { return };
        self.on_screen = true;
        let tx = self.tx.clone();
        post(move || {
            let _ = tx.send(run(request));
        });
    }

    /// Answer of a finished dialog, or `None` while none has come back.
    pub fn poll(&mut self) -> Option<Answer> {
        match self.rx.try_recv() {
            Ok(answer) => {
                self.on_screen = false;
                Some(answer)
            }
            Err(_) => None,
        }
    }
}

/// Show one dialog and turn the choice into an `Answer`. Runs on the main
/// thread in every case (`post`'s contract), which `NSOpenPanel` requires.
fn run(request: Request) -> Answer {
    match request {
        Request::Rom { start } => match crate::picker::pick_rom(&start) {
            Some(path) => Answer::Rom(path),
            None => Answer::Cancelled,
        },
        Request::LibraryDir { current } => {
            match crate::picker::pick_dir("Dossier des ROMs", &current) {
                Some(dir) => Answer::LibraryDir(dir),
                None => Answer::Cancelled,
            }
        }
        Request::ScreenshotDir { current } => {
            match crate::picker::pick_dir("Dossier des captures", &current) {
                Some(dir) => Answer::ScreenshotDir(dir),
                None => Answer::Cancelled,
            }
        }
        Request::SaveDir { current } => {
            match crate::picker::pick_dir("Dossier des sauvegardes", &current) {
                Some(dir) => Answer::SaveDir(dir),
                None => Answer::Cancelled,
            }
        }
    }
}

/// Run `work` on the main thread, outside the current call stack (see module
/// docs).
#[cfg(target_os = "macos")]
fn post(work: impl FnOnce() + 'static) {
    main_queue::post(work);
}

/// Elsewhere the event loop has no re-entrancy guard to trip, so the dialog
/// runs inline on the calling (main) thread.
#[cfg(not(target_os = "macos"))]
fn post(work: impl FnOnce() + 'static) {
    work();
}

#[cfg(target_os = "macos")]
mod main_queue {
    use std::ffi::c_void;

    /// Opaque libdispatch object; only its address is ever used.
    #[repr(C)]
    struct DispatchObject {
        _private: [u8; 0],
    }

    // libdispatch lives in libSystem, always linked on macOS.
    // `dispatch_get_main_queue()` is a C macro over the `_dispatch_main_q`
    // symbol, so the queue is referenced directly here.
    extern "C" {
        static _dispatch_main_q: DispatchObject;
        fn dispatch_async_f(
            queue: *mut c_void,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
    }

    extern "C" fn trampoline(context: *mut c_void) {
        // SAFETY: `context` is the box leaked by `post` below, and libdispatch
        // runs a submitted function exactly once.
        let work: Box<Box<dyn FnOnce()>> =
            unsafe { Box::from_raw(context as *mut Box<dyn FnOnce()>) };
        (*work)();
    }

    /// Submit `work` to the main queue: it runs on the main thread, from the
    /// run loop, once the current call stack has unwound.
    pub fn post(work: impl FnOnce() + 'static) {
        let boxed: Box<Box<dyn FnOnce()>> = Box::new(Box::new(work));
        let context = Box::into_raw(boxed) as *mut c_void;
        // SAFETY: `_dispatch_main_q` is a valid dispatch queue for the lifetime
        // of the process and `context` is a live box handed to `trampoline`.
        unsafe {
            dispatch_async_f(
                &_dispatch_main_q as *const DispatchObject as *mut c_void,
                context,
                trampoline,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_second_request_is_dropped_while_one_is_pending() {
        let mut dialogs = Dialogs::new();
        assert!(!dialogs.is_busy());
        dialogs.request(Request::Rom { start: PathBuf::from("roms") });
        assert!(dialogs.is_busy());
        dialogs.request(Request::LibraryDir { current: PathBuf::from("/other") });
        assert_eq!(dialogs.queued, Some(Request::Rom { start: PathBuf::from("roms") }));
        assert_eq!(dialogs.poll(), None, "nothing has been answered yet");
    }

    #[test]
    fn an_answer_clears_the_pending_state_exactly_once() {
        let mut dialogs = Dialogs::new();
        dialogs.queued = None;
        dialogs.on_screen = true;
        dialogs.tx.send(Answer::LibraryDir(PathBuf::from("/games"))).expect("send");
        assert_eq!(dialogs.poll(), Some(Answer::LibraryDir(PathBuf::from("/games"))));
        assert!(!dialogs.is_busy(), "the shell must be free to ask again");
        assert_eq!(dialogs.poll(), None);
    }

    /// The whole point of the module: no native modal may be opened from a
    /// winit callback (see the module docs and `docs/PUNCHLIST.md`). `video.rs`
    /// is the file that *is* the winit callbacks, so it must not name the
    /// picker or rfd at all — it queues a `Request` instead.
    #[test]
    fn the_event_loop_never_calls_the_native_picker_itself() {
        let video = include_str!("video.rs");
        assert!(!video.contains("picker::"), "video.rs must go through dialog::Request");
        assert!(!video.contains("rfd::"), "video.rs must not open a native modal");
    }

    #[test]
    fn every_request_maps_to_its_own_answer_variant() {
        // Pure shape check: `run` itself needs a display, but the pairing of
        // request and answer is what the event loop dispatches on.
        let answers = [
            Answer::Rom(PathBuf::from("/roms/a.sfc")),
            Answer::LibraryDir(PathBuf::from("/roms")),
            Answer::ScreenshotDir(PathBuf::from("/shots")),
            Answer::SaveDir(PathBuf::from("/saves")),
            Answer::Cancelled,
        ];
        let mut seen = 0;
        for answer in &answers {
            match answer {
                Answer::Rom(p) => assert!(p.extension().is_some()),
                Answer::LibraryDir(p) | Answer::ScreenshotDir(p) | Answer::SaveDir(p) => {
                    assert!(p.is_absolute() || p == Path::new("roms"));
                }
                Answer::Cancelled => {}
            }
            seen += 1;
        }
        assert_eq!(seen, answers.len());
    }
}
