//! Windowed video output: winit window + pixels framebuffer, 256x224
//! BGR555 -> RGBA upload, 50.007/60.0988 fps pacing at absolute deadlines.
//!
//! Frame cadence is paced by wall-clock deadlines rather than vsync (which is
//! disabled): each `about_to_wait` computes the next presentation deadline,
//! sleeps for the bulk of the remaining time, then spin-waits the last
//! `SPIN_SLACK` for sub-millisecond accuracy (OS `sleep()` granularity is
//! coarse — a few ms on some hosts — so a plain sleep-to-deadline would
//! frequently overshoot).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pixels::{Pixels, SurfaceTexture};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use snes_core::{Cartridge, JoypadState, Snes, SCREEN_HEIGHT, SCREEN_WIDTH};

use crate::audio::AudioOutput;
use crate::input;
#[cfg(target_os = "macos")]
use crate::menu::{self, AppMenu};
use crate::picker;
use crate::save;

/// Integer upscale factor for the 256x224 native framebuffer.
pub const WINDOW_SCALE: u32 = 3;

/// Wall-clock slack reserved for the spin-wait tail of each frame's pacing
/// deadline (see module docs).
const SPIN_SLACK: Duration = Duration::from_micros(1200);

/// Run the windowed frontend (M5): winit event loop + pixels present, paced
/// to the cartridge region's native field rate (PAL 50.007 Hz / NTSC
/// 60.0988 Hz, from `Region::frames_per_second`) via an absolute deadline.
///
/// `save_path`/`sram_baseline` come from `save::load_sram` (already applied
/// to `cart.sram` by the caller); battery SRAM is written back to
/// `save_path` once the event loop exits, however it exits (window close,
/// Esc, or a fatal window/surface creation error), since `app` is still
/// owned here after `run_app` returns. The `O` hotkey can swap in a
/// different ROM mid-session (see `App::open_rom_dialog`); `App` then owns
/// its own current `save_path`/`sram_baseline` so the exit-time save below
/// always targets whichever game is loaded when the window closes.
pub fn run(
    rom_path: PathBuf,
    cart: Cartridge,
    save_path: PathBuf,
    sram_baseline: Vec<u8>,
) -> Result<(), String> {
    let title = format!("snes-frontend - {}", cart.title.trim());
    let region = cart.region;
    let snes = Snes::new(cart);
    let frame_duration = Duration::from_secs_f64(1.0 / region.frames_per_second());

    let mut event_loop_builder = EventLoop::builder();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        // winit creates its own default NSApp main menu unless told not to;
        // left enabled it would duplicate (and fight over) the muda-built
        // menu bar installed in `App::resumed`.
        event_loop_builder.with_default_menu(false);
    }
    let event_loop = event_loop_builder.build().map_err(|e| format!("create event loop: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Poll);

    // Audio is best-effort: a missing device must never fail the emulator.
    let audio = AudioOutput::new();

    let mut app = App {
        title,
        snes,
        current_rom_path: rom_path,
        save_path,
        sram_baseline,
        frame_duration,
        next_deadline: Instant::now() + frame_duration,
        window: None,
        pixels: None,
        pad: JoypadState::default(),
        paused: false,
        frame_advance: false,
        audio,
        audio_scratch: Vec::new(),
        #[cfg(target_os = "macos")]
        menu: None,
    };
    let result = event_loop.run_app(&mut app).map_err(|e| format!("event loop: {e}"));
    save::save_if_dirty(&app.snes.bus.cart, &app.save_path, &app.sram_baseline);
    result
}

struct App {
    title: String,
    snes: Snes,
    /// Path of the currently loaded ROM (updated by `switch_rom`); used by
    /// `Emulation > Reset` to reload the same cart.
    current_rom_path: PathBuf,
    /// Sidecar `.srm` path for the currently loaded cart (updated by the `O`
    /// hotkey when the ROM is switched).
    save_path: PathBuf,
    /// Post-load SRAM snapshot for the currently loaded cart; see
    /// `save::load_sram`/`save::save_if_dirty`.
    sram_baseline: Vec<u8>,
    frame_duration: Duration,
    /// Absolute wall-clock time the next emulated frame should be presented at.
    next_deadline: Instant,
    window: Option<Arc<Window>>,
    pixels: Option<Pixels<'static>>,
    /// Player-1 pad state accumulated from keyboard events; player 2 is
    /// unconnected (frontend has no multi-controller UI yet).
    pad: JoypadState,
    paused: bool,
    /// Set by `N` while paused: step exactly one frame, then cleared.
    frame_advance: bool,
    /// cpal output; `None` when no audio device was available.
    audio: Option<AudioOutput>,
    /// Reused per-frame drain buffer to avoid re-allocating each frame.
    audio_scratch: Vec<(i16, i16)>,
    /// Menu bar handles, installed once in `resumed` (needs `NSApp` to
    /// exist first); `None` until then.
    #[cfg(target_os = "macos")]
    menu: Option<AppMenu>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // Already initialized; e.g. a redundant resume on some platforms.
        }
        let size = LogicalSize::new(
            SCREEN_WIDTH as u32 * WINDOW_SCALE,
            SCREEN_HEIGHT as u32 * WINDOW_SCALE,
        );
        let attrs = Window::default_attributes().with_title(self.title.clone()).with_inner_size(size);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("error: create window: {e}");
                event_loop.exit();
                return;
            }
        };
        let phys = window.inner_size();
        let surface_texture = SurfaceTexture::new(phys.width, phys.height, Arc::clone(&window));
        let mut pixels =
            match Pixels::new(SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32, surface_texture) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error: create pixels surface: {e}");
                    event_loop.exit();
                    return;
                }
            };
        // Frame pacing is done manually against a wall-clock deadline; vsync
        // would additionally block on the compositor's own refresh cycle.
        pixels.enable_vsync(false);
        self.window = Some(window);
        self.pixels = Some(pixels);
        self.next_deadline = Instant::now() + self.frame_duration;

        // NSApp only exists once winit has resumed at least once; installing
        // the menu bar any earlier is a silent no-op on macOS (see `menu`
        // module docs).
        #[cfg(target_os = "macos")]
        {
            self.menu = Some(menu::install());
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _window_id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    if let Some(pixels) = &mut self.pixels {
                        // pixels' scaling renderer keeps the 256x224 buffer at
                        // the nearest integer scale, letterboxing any remainder.
                        let _ = pixels.resize_surface(size.width, size.height);
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { physical_key: PhysicalKey::Code(code), state, repeat, .. },
                ..
            } => self.handle_key(event_loop, code, state, repeat),
            WindowEvent::RedrawRequested => {
                if let (Some(window), Some(pixels)) = (&self.window, &self.pixels) {
                    window.pre_present_notify();
                    if let Err(e) = pixels.render() {
                        eprintln!("error: pixels render: {e}");
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            return; // Not yet resumed on this platform.
        }
        #[cfg(target_os = "macos")]
        self.poll_menu_events(event_loop);
        pace(&mut self.next_deadline, self.frame_duration);

        if !self.paused || self.frame_advance {
            self.snes.run_frame([self.pad, JoypadState::default()]);
            self.frame_advance = false;
            if let Some(pixels) = &mut self.pixels {
                self.snes.framebuffer.to_rgba(pixels.frame_mut());
            }
            // Feed this frame's audio into the ring; the callback's rate control
            // absorbs the emulator/host clock drift.
            if let Some(audio) = &mut self.audio {
                self.audio_scratch.clear();
                self.snes.drain_audio(&mut self.audio_scratch);
                audio.push(&self.audio_scratch);
            }
        }
        // Always request a redraw, even while paused, so the compositor keeps
        // presenting the last frame (e.g. after an expose/resize).
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::Poll);
    }
}

impl App {
    fn handle_key(&mut self, event_loop: &ActiveEventLoop, code: KeyCode, state: ElementState, repeat: bool) {
        let pressed = state == ElementState::Pressed;
        // Hotkeys act on the initial press only (ignore key-repeat).
        if pressed && !repeat {
            match code {
                KeyCode::Escape => {
                    event_loop.exit();
                    return;
                }
                KeyCode::KeyP => {
                    self.paused = !self.paused;
                    return;
                }
                KeyCode::KeyN => {
                    if self.paused {
                        self.frame_advance = true;
                    }
                    return;
                }
                KeyCode::KeyO => {
                    self.open_rom_dialog();
                    return;
                }
                KeyCode::F5 => {
                    self.save_state();
                    return;
                }
                KeyCode::F9 => {
                    self.load_state();
                    return;
                }
                _ => {}
            }
        }
        if let Some(name) = input::keycode_to_button(code) {
            let _ = input::set_button(&mut self.pad, name, pressed);
        }
    }

    /// `O` hotkey: open the native ROM picker and, if a file was chosen,
    /// tear down the running game (saving its SRAM first) and start the
    /// picked one. Cancelling the dialog or a load error leaves the current
    /// game running untouched.
    fn open_rom_dialog(&mut self) {
        let Some(path) = picker::pick_rom() else {
            return; // Cancelled: keep playing the current game.
        };
        if let Err(e) = self.switch_rom(&path) {
            eprintln!("error: could not load {}: {e}", path.display());
        }
    }

    /// F5 / `Emulation > Save State` (Cmd+S): snapshot the whole console
    /// (`Snes::save_state`) to the `<rom>.state` sidecar (slot 0) next to the
    /// currently loaded ROM. Never fails the run: an I/O error is reported and
    /// emulation continues.
    fn save_state(&mut self) {
        let path = crate::state::state_path(&self.current_rom_path, 0);
        let bytes = self.snes.save_state();
        match std::fs::write(&path, &bytes) {
            Ok(()) => eprintln!("state: saved {} ({} bytes)", path.display(), bytes.len()),
            Err(e) => eprintln!("state: could not write {}: {e}", path.display()),
        }
    }

    /// F9 / `Emulation > Load State` (Cmd+L): restore the console from the
    /// `<rom>.state` sidecar (slot 0). The blob carries no ROM image;
    /// `Snes::load_state` reattaches the live ROM and rejects a state saved
    /// from a different game. Any error (missing file, wrong ROM, corrupt
    /// blob) is reported and the running game is left untouched.
    fn load_state(&mut self) {
        let path = crate::state::state_path(&self.current_rom_path, 0);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("state: could not read {}: {e}", path.display());
                return;
            }
        };
        match self.snes.load_state(&bytes) {
            Ok(()) => eprintln!("state: loaded {}", path.display()),
            Err(e) => eprintln!("state: load failed ({}): {e}", path.display()),
        }
    }

    /// Replace `self.snes` with a freshly constructed console for the ROM at
    /// `path`. Persists the outgoing cart's SRAM (via its own `save_path`)
    /// before replacing it, then loads the new cart's `.srm` sidecar the
    /// same way startup does, resets pad/pause/frame-advance state, and
    /// retargets pacing at the new cart's region field rate (a game switch
    /// can cross the PAL/NTSC line).
    fn switch_rom(&mut self, path: &Path) -> Result<(), String> {
        save::save_if_dirty(&self.snes.bus.cart, &self.save_path, &self.sram_baseline);

        let bytes = crate::load_rom_bytes(path)?;
        let mut cart = Cartridge::from_bytes(bytes)?;
        let save_path = save::default_save_path(path);
        let sram_baseline = save::load_sram(&mut cart, &save_path);

        self.title = format!("snes-frontend - {}", cart.title.trim());
        self.frame_duration = Duration::from_secs_f64(1.0 / cart.region.frames_per_second());
        self.snes = Snes::new(cart);
        self.save_path = save_path;
        self.sram_baseline = sram_baseline;
        self.pad = JoypadState::default();
        self.paused = false;
        self.frame_advance = false;
        self.next_deadline = Instant::now() + self.frame_duration;
        if let Some(window) = &self.window {
            window.set_title(&self.title);
        }
        self.current_rom_path = path.to_path_buf();
        Ok(())
    }

    /// `Emulation > Reset` menu item (Cmd+R): reload the currently running
    /// ROM in place. Reuses `switch_rom` with the same path rather than
    /// rebuilding `Snes` from `self.snes.bus.cart` directly (`Cartridge`
    /// isn't `Clone`): `switch_rom` first flushes the live, possibly-dirty
    /// SRAM to `save_path`, then reloads that same file into the fresh
    /// cart, so the net effect is a power-on reset of CPU/PPU/APU state
    /// that preserves the current battery save — matching the SNES's
    /// physical reset button, which restarts execution but never erases
    /// cartridge SRAM.
    #[cfg(target_os = "macos")]
    fn reset(&mut self) {
        let path = self.current_rom_path.clone();
        if let Err(e) = self.switch_rom(&path) {
            eprintln!("error: reset failed to reload {}: {e}", path.display());
        }
    }

    /// Drains muda's global menu-click channel (populated on the main
    /// thread by AppKit when a menu item is activated — either by mouse or
    /// by its accelerator) and dispatches each click. Called once per
    /// `about_to_wait` so a menu action lands before that iteration's
    /// pacing/frame-run, the same way keyboard hotkeys are handled
    /// synchronously in `window_event`. `About` is a muda `PredefinedMenuItem`
    /// that AppKit runs itself (standard about panel) and never appears on this
    /// channel. `Quit` is a *custom* item (not `PredefinedMenuItem::quit`) so
    /// it routes here and we exit the winit loop the same way `Esc`/window-close
    /// do, which triggers the exit-time battery-SRAM flush in `run` — AppKit's
    /// `terminate:` would kill the process before that save could run.
    #[cfg(target_os = "macos")]
    fn poll_menu_events(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
            let Some(menu) = &self.menu else { continue };
            // Quit shares one id across the app-menu and File-menu items.
            if event.id == menu.quit.id() || event.id == menu.quit_file.id() {
                event_loop.exit();
            } else if event.id == menu.open_rom.id() {
                self.open_rom_dialog();
            } else if event.id == menu.pause_resume.id() {
                self.paused = !self.paused;
            } else if event.id == menu.reset.id() {
                self.reset();
            } else if event.id == menu.save_state.id() {
                self.save_state();
            } else if event.id == menu.load_state.id() {
                self.load_state();
            }
        }
    }
}

/// Sleep for the bulk of the remaining time until `deadline`, then spin the
/// last `SPIN_SLACK` for accuracy; advances `deadline` by one
/// `frame_duration`. If wall clock has drifted more than 4 frames past the
/// deadline (long pause, breakpoint, laptop sleep), resync instead of
/// fast-forwarding a backlog of frames.
fn pace(deadline: &mut Instant, frame_duration: Duration) {
    let now = Instant::now();
    if now < *deadline {
        let remaining = *deadline - now;
        if remaining > SPIN_SLACK {
            std::thread::sleep(remaining - SPIN_SLACK);
        }
        while Instant::now() < *deadline {
            std::hint::spin_loop();
        }
    }
    *deadline += frame_duration;
    if Instant::now() > *deadline + frame_duration * 4 {
        *deadline = Instant::now() + frame_duration;
    }
}
