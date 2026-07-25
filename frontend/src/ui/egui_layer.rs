//! egui on top of the `pixels` surface.
//!
//! How the two cohabit: `pixels` owns the wgpu instance, adapter, device,
//! queue and surface, and exposes them through `Pixels::render_with`, which
//! hands the caller a `CommandEncoder` and the swap-chain `TextureView` for
//! the frame before submitting the encoder itself. `egui-wgpu` needs exactly
//! those three things, so no second device/surface is created: `EguiLayer`
//! borrows `pixels`' device/queue to upload its font and mesh buffers, then
//! records one extra render pass into `pixels`' own encoder, targeting
//! `pixels`' own view.
//!
//! That is only possible because both crates compile against the *same* wgpu:
//! `pixels` 0.16 and `egui-wgpu` 0.33 both require `wgpu ^27`, so cargo
//! unifies them and the `Device`/`Queue`/`TextureView` types are literally the
//! same types. (This is why `pixels` had to move from 0.15 — its wgpu 0.19 has
//! no `egui-wgpu` release that also targets winit 0.30.)
//!
//! The extra pass loads (`LoadOp::Load`) when the emulated picture has already
//! been drawn by `pixels`' scaling renderer, so the UI composites over the
//! game; it clears (`LoadOp::Clear`) when egui owns the whole window (home
//! screen), so no stale framebuffer shows through.

use std::sync::Arc;

use egui_wgpu::wgpu;
use winit::event::WindowEvent;
use winit::window::Window;

/// egui context + winit input translation + wgpu paint backend for one window.
pub struct EguiLayer {
    /// winit -> egui event translation, and the egui context it feeds.
    state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    /// Tessellated output of the last `run`, consumed by the next `render`.
    paint_jobs: Vec<egui::ClippedPrimitive>,
    textures_delta: egui::TexturesDelta,
    screen: egui_wgpu::ScreenDescriptor,
}

impl EguiLayer {
    /// `format` must be the format of the view `render` will be given —
    /// `Pixels::surface_texture_format()`. `size` is the surface size in
    /// physical pixels and `pixels_per_point` the window's scale factor.
    pub fn new(
        window: &Arc<Window>,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: (u32, u32),
        pixels_per_point: f32,
    ) -> Self {
        let ctx = egui::Context::default();
        super::theme::apply(&ctx);
        let state = egui_winit::State::new(
            ctx,
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(pixels_per_point),
            None,
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        let renderer = egui_wgpu::Renderer::new(
            device,
            format,
            egui_wgpu::RendererOptions {
                // The UI is flat 2D: no MSAA, no depth buffer. Dithering only
                // helps large smooth gradients, which this style has none of.
                msaa_samples: 1,
                depth_stencil_format: None,
                dithering: false,
                ..Default::default()
            },
        );
        Self {
            state,
            renderer,
            paint_jobs: Vec::new(),
            textures_delta: egui::TexturesDelta::default(),
            screen: egui_wgpu::ScreenDescriptor {
                size_in_pixels: [size.0.max(1), size.1.max(1)],
                pixels_per_point,
            },
        }
    }

    /// Feed a window event to egui. The returned `consumed` flag tells the
    /// caller whether egui used the event (a click on a widget, a keystroke in
    /// a focused text field); the game screen ignores it, since the emulated
    /// pad must never lose a key to an invisible UI.
    pub fn on_window_event(
        &mut self,
        window: &Arc<Window>,
        event: &WindowEvent,
    ) -> egui_winit::EventResponse {
        self.state.on_window_event(window, event)
    }

    /// Track the surface size; `pixels_per_point` is refreshed from egui's own
    /// per-frame value in `run`, so only the pixel size is set here.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.screen.size_in_pixels = [width.max(1), height.max(1)];
    }

    /// Build one UI frame. `build` returns whatever the caller needs out of
    /// the UI (here: the requested `Action`).
    pub fn run<T>(
        &mut self,
        window: &Arc<Window>,
        mut build: impl FnMut(&egui::Context) -> T,
    ) -> T {
        let input = self.state.take_egui_input(window);
        let mut produced = None;
        let output = self.state.egui_ctx().clone().run(input, |ctx| {
            produced = Some(build(ctx));
        });
        self.state.handle_platform_output(window, output.platform_output);
        self.paint_jobs =
            self.state.egui_ctx().tessellate(output.shapes, output.pixels_per_point);
        self.textures_delta.append(output.textures_delta);
        self.screen.pixels_per_point = output.pixels_per_point;
        // `run`'s closure is always called exactly once by egui.
        produced.expect("egui::Context::run did not invoke the UI closure")
    }

    /// Record the last built frame into `encoder`, drawing into `target`.
    /// `clear` replaces whatever is already in the target (home screen);
    /// `None` composites over it (UI on top of the game).
    pub fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        clear: Option<[f64; 4]>,
    ) {
        for (id, delta) in &self.textures_delta.set {
            self.renderer.update_texture(device, queue, *id, delta);
        }
        // These command buffers stage buffer/texture uploads the render pass
        // below reads, so they must reach the queue first; `pixels` submits
        // `encoder` itself once this closure returns.
        let uploads =
            self.renderer.update_buffers(device, queue, encoder, &self.paint_jobs, &self.screen);
        if !uploads.is_empty() {
            queue.submit(uploads);
        }
        {
            let load = match clear {
                Some([r, g, b, a]) => wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }),
                None => wgpu::LoadOp::Load,
            };
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("prisme-egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            self.renderer.render(&mut pass, &self.paint_jobs, &self.screen);
        }
        for id in &self.textures_delta.free {
            self.renderer.free_texture(id);
        }
        self.textures_delta.clear();
    }
}
