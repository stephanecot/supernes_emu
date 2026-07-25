//! Application shell: the egui layer, the screen state machine, the Prisme
//! visual identity and the screens themselves.
//!
//! Split so the parts that need no display can be unit-tested: `app_state`
//! (transitions, Escape semantics), `theme` (palette, style application on a
//! headless `egui::Context`) and the decision helpers of `library_view` /
//! `game_sheet` / `settings` (card subtitle, sheet facts, offered choices,
//! Escape precedence) are pure logic; `egui_layer` is the only module that
//! touches wgpu; `home`, `library_view`, `game_sheet` and `settings` only
//! build widgets, and `textures` caches the decoded pictures they draw.

pub mod app_state;
pub mod confirm;
pub mod egui_layer;
pub mod game;
pub mod game_sheet;
pub mod home;
pub mod library_view;
pub mod settings;
pub mod textures;
pub mod theme;

pub use app_state::{escape_action, Action, AppState, EscapeAction, Screen, Setting};
pub use egui_layer::EguiLayer;
pub use library_view::LibraryUi;
pub use settings::SettingsUi;
pub use textures::TextureStore;
