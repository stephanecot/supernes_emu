//! Windowed video output: winit window + pixels framebuffer, 256x224
//! BGR555 -> RGBA upload, 50.007/60.0988 fps pacing at absolute deadlines.
//!
//! Frame cadence is paced by wall-clock deadlines rather than vsync (which is
//! disabled): each `about_to_wait` computes the next presentation deadline,
//! sleeps for the bulk of the remaining time, then spin-waits the last
//! `SPIN_SLACK` for sub-millisecond accuracy (OS `sleep()` granularity is
//! coarse — a few ms on some hosts — so a plain sleep-to-deadline would
//! frequently overshoot).

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

/// Integer upscale factor for the 256x224 native framebuffer.
pub const WINDOW_SCALE: u32 = 3;

/// Wall-clock slack reserved for the spin-wait tail of each frame's pacing
/// deadline (see module docs).
const SPIN_SLACK: Duration = Duration::from_micros(1200);

/// Run the windowed frontend (M5): winit event loop + pixels present, paced
/// to the cartridge region's native field rate (PAL 50.007 Hz / NTSC
/// 60.0988 Hz, from `Region::frames_per_second`) via an absolute deadline.
pub fn run(cart: Cartridge) -> Result<(), String> {
    let title = format!("snes-frontend - {}", cart.title.trim());
    let region = cart.region;
    let snes = Snes::new(cart);
    let frame_duration = Duration::from_secs_f64(1.0 / region.frames_per_second());

    let event_loop = EventLoop::new().map_err(|e| format!("create event loop: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Poll);

    // Audio is best-effort: a missing device must never fail the emulator.
    let audio = AudioOutput::new();

    let mut app = App {
        title,
        snes,
        frame_duration,
        next_deadline: Instant::now() + frame_duration,
        window: None,
        pixels: None,
        pad: JoypadState::default(),
        paused: false,
        frame_advance: false,
        audio,
        audio_scratch: Vec::new(),
    };
    event_loop.run_app(&mut app).map_err(|e| format!("event loop: {e}"))
}

struct App {
    title: String,
    snes: Snes,
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
                _ => {}
            }
        }
        if let Some(name) = input::keycode_to_button(code) {
            let _ = input::set_button(&mut self.pad, name, pressed);
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
