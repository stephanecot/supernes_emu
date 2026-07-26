//! GPU textures for the pictures the library shows (generated thumbnails and
//! the player's own screenshots).
//!
//! egui uploads a texture through the `egui::Context`, which only exists
//! inside a UI frame, so the store is filled lazily from the UI and keeps its
//! handles between frames — an immediate-mode UI would otherwise re-upload
//! every picture 60 times a second.
//!
//! A failed load (missing file, unsupported PNG) is memoized as `None` so a
//! broken file is decoded once, not once per frame. `forget` drops one entry,
//! which is what makes a regenerated or newly promoted thumbnail appear
//! without restarting.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How many pictures stay resident. A 256x224 RGBA texture is 224 KB, so the
/// cap bounds the store to about 27 MB of GPU memory whatever the size of the
/// library; past it, the least recently drawn picture is dropped rather than
/// refusing to load new ones (a library larger than the cap would otherwise
/// show permanent placeholders past the cap-th game).
pub const MAX_TEXTURES: usize = 120;

/// One cached picture and the tick it was last drawn at.
struct Entry {
    /// `None` marks a file that failed to load, so it is not retried.
    texture: Option<egui::TextureHandle>,
    used: u64,
}

#[derive(Default)]
pub struct TextureStore {
    entries: HashMap<PathBuf, Entry>,
    /// Monotonic use counter driving the eviction order.
    clock: u64,
}

impl TextureStore {
    /// Key the suspended session's frame is uploaded under. Not a real path:
    /// nothing on disk holds this picture, and the store only ever compares
    /// keys.
    pub const SESSION_FRAME: &str = "<session-frame>";

    pub fn new() -> Self {
        Self::default()
    }

    /// Texture for `path`, decoding and uploading it on first use.
    pub fn get(&mut self, ctx: &egui::Context, path: &Path) -> Option<&egui::TextureHandle> {
        self.clock += 1;
        if !self.entries.contains_key(path) {
            if self.entries.len() >= MAX_TEXTURES {
                self.evict_oldest();
            }
            let texture = Self::load(ctx, path);
            self.entries.insert(path.to_path_buf(), Entry { texture, used: self.clock });
        }
        let clock = self.clock;
        let entry = self.entries.get_mut(path)?;
        entry.used = clock;
        entry.texture.as_ref()
    }

    /// Texture holding one 256x224 frame straight from the console, uploaded
    /// under `key` and replaced whenever `frame` differs from what is there.
    ///
    /// The store is otherwise keyed by file path, because everything else it
    /// holds is a picture on disk. The suspended session's last frame is not:
    /// it exists only in memory, and writing a PNG on every trip to the home
    /// screen to make it fit the existing shape would be a file written for
    /// the sake of an interface.
    pub fn frame(
        &mut self,
        ctx: &egui::Context,
        key: &Path,
        frame: &[u8],
        size: [usize; 2],
    ) -> Option<&egui::TextureHandle> {
        if frame.len() != size[0] * size[1] * 4 {
            return None;
        }
        self.clock += 1;
        let image = egui::ColorImage::from_rgba_unmultiplied(size, frame);
        match self.entries.get_mut(key) {
            // `set` re-uploads in place, so a paused session that is left and
            // re-entered does not leak a texture per visit.
            Some(entry) => {
                if let Some(texture) = &mut entry.texture {
                    texture.set(image, egui::TextureOptions::NEAREST);
                }
                entry.used = self.clock;
            }
            None => {
                if self.entries.len() >= MAX_TEXTURES {
                    self.evict_oldest();
                }
                let texture =
                    ctx.load_texture(key.to_string_lossy(), image, egui::TextureOptions::NEAREST);
                self.entries
                    .insert(key.to_path_buf(), Entry { texture: Some(texture), used: self.clock });
            }
        }
        self.entries.get(key)?.texture.as_ref()
    }

    /// Drop the least recently drawn picture. Called only when the store is
    /// full, so at most one decode is re-done if the player scrolls back.
    fn evict_oldest(&mut self) {
        let oldest = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.used)
            .map(|(path, _)| path.clone());
        if let Some(path) = oldest {
            self.entries.remove(&path);
        }
    }

    /// Drop a cached entry (including a memoized failure) so the next `get`
    /// reloads it from disk.
    pub fn forget(&mut self, path: &Path) {
        self.entries.remove(path);
    }

    /// Number of cached entries, successes and failures alike. Test-only
    /// introspection: the UI never needs it, and the cap is enforced inside
    /// `get`.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn load(ctx: &egui::Context, path: &Path) -> Option<egui::TextureHandle> {
        let (w, h, rgba) = match crate::thumbs::decode_png(path) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("library: {e}");
                return None;
            }
        };
        if rgba.len() < w as usize * h as usize * 4 {
            return None;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        // Nearest-neighbour: these are pixel-art pictures, and the grid shows
        // them smaller than native, where bilinear would blur them.
        Some(ctx.load_texture(
            path.to_string_lossy().into_owned(),
            image,
            egui::TextureOptions::NEAREST,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame of a suspended session is uploaded from memory and *replaced*
    /// in place on every visit to the home screen: a new texture per visit
    /// would leak one for each trip in and out of a game.
    #[test]
    fn a_session_frame_is_uploaded_once_and_then_replaced() {
        let ctx = egui::Context::default();
        let mut store = TextureStore::new();
        let key = Path::new(TextureStore::SESSION_FRAME);
        let size = [4, 2];
        let green = vec![0, 255, 0, 255].repeat(8);
        let red = vec![255, 0, 0, 255].repeat(8);

        assert!(store.frame(&ctx, key, &green, size).is_some());
        assert_eq!(store.len(), 1);
        assert!(store.frame(&ctx, key, &red, size).is_some());
        assert_eq!(store.len(), 1, "the second frame replaces the first");

        // A buffer that does not match the size it claims is refused rather
        // than uploaded as garbage.
        assert!(store.frame(&ctx, key, &green, [8, 8]).is_none());
    }


    /// Unique scratch directory per test — no test may share a path with
    /// another test or with another run (they run concurrently). Nothing is
    /// ever created here: the tests only need paths that fail to load.
    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("prisme_tex_{}_{}", std::process::id(), tag))
    }

    #[test]
    fn a_fresh_store_is_empty_and_forgetting_is_harmless() {
        let mut store = TextureStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        store.forget(Path::new("/nowhere.png"));
        assert!(store.is_empty());
    }

    #[test]
    fn a_missing_file_is_memoized_as_a_failure_instead_of_retried() {
        // `egui::Context` is pure CPU state, so this runs with no window: the
        // load fails at the decode step, before any GPU work.
        let ctx = egui::Context::default();
        let mut store = TextureStore::new();
        let missing = scratch("missing").join("thumbnail.png");
        assert!(store.get(&ctx, &missing).is_none());
        assert_eq!(store.len(), 1, "the failure must be cached");
        assert!(store.get(&ctx, &missing).is_none());
        assert_eq!(store.len(), 1);
        store.forget(&missing);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn the_store_never_grows_past_the_cap_and_evicts_the_oldest() {
        let ctx = egui::Context::default();
        let mut store = TextureStore::new();
        let dir = scratch("cap");
        // Every path fails to decode (no such file), which is enough to
        // exercise the bookkeeping: an entry is stored either way.
        let path = |i: usize| dir.join(format!("{i}.png"));
        for i in 0..MAX_TEXTURES {
            store.get(&ctx, &path(i));
        }
        assert_eq!(store.len(), MAX_TEXTURES);
        // Touch entry 0 so it is no longer the least recently used, then push
        // one more: entry 1 must be the one evicted.
        store.get(&ctx, &path(0));
        store.get(&ctx, &path(MAX_TEXTURES));
        assert_eq!(store.len(), MAX_TEXTURES, "the cap must hold");
        assert!(store.entries.contains_key(&path(0)));
        assert!(!store.entries.contains_key(&path(1)));
        assert!(store.entries.contains_key(&path(MAX_TEXTURES)));
    }
}
