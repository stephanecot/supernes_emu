//! Box art and catalogue facts for a game, from sources that need no account
//! and no API key. Design and measured source figures: `docs/METADATA.md`.
//!
//! **The chain, and why it is in this order.**
//!
//! 1. **CRC32 of the de-headered image** (`Cartridge::rom`, the copier header
//!    already stripped by the loader). Computed at scan time, where the ROM is
//!    in memory anyway, and stored on `library::GameEntry`.
//! 2. **No-Intro DAT** turns that CRC into a **canonical name**, with certainty
//!    rather than resemblance: `Super Mario Kart (Europe)`. A dump that is not
//!    in it — a translation patch, a trainer, homebrew — is not an error; it
//!    keeps its sheet and is told so.
//! 3. **Facts** from the other `libretro-database` categories. The design
//!    expected these to be keyed by the canonical name; they are in fact keyed
//!    by the **same CRC** (`rom ( crc XXXXXXXX )`, with the canonical name only
//!    in a `comment`), which removes the last place a name could be matched
//!    approximately.
//! 4. **Description** from the Wikipedia REST summary API — the only keyless
//!    source of prose, since `libretro-database` has none at all. This is the
//!    one step matched **by title**, which is why the sheet attributes it
//!    visibly (see `Description`).
//! 5. **Box art** from `libretro-thumbnails`, `Named_Boxarts/<canonical
//!    name>.png` — reliable precisely because that name came from the CRC.
//!
//! **What is downloaded once.** The nine catalogue files are a few hundred
//! kilobytes each and cover four thousand games; they are written to
//! `<config dir>/Prisme/Catalog/` on the first fetch and read from disk
//! forever after, so a library of any size costs one round trip per file
//! rather than one per game. Per-game results live in
//! `<config dir>/Prisme/metadata.json` (see `Store` for why not `prefs.json`)
//! and box art in `<config dir>/Prisme/Boxarts/<game id>.png`.
//!
//! **Nothing here fails loudly.** Every step that cannot answer returns `None`
//! and the sheet keeps what it had; the only thing a failed fetch may change is
//! nothing at all.
//!
//! Everything except `Catalog::load` and `fill` is pure logic over plain data,
//! and those two take their network access as a `net::Fetch`, so the whole
//! chain is exercised offline in this module's own tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::i18n::{Lang, Msg};
use crate::net::{self, Fetch};

/// Directory the downloaded catalogue files are kept in, under the app's
/// config directory.
pub const CATALOG_DIR: &str = "Catalog";
/// Directory the downloaded box art is kept in.
pub const BOXART_DIR: &str = "Boxarts";
/// Per-game results, beside `library.json`.
pub const CACHE_FILE: &str = "metadata.json";
/// Layout version of `metadata.json`; another value is discarded and refetched.
pub const CACHE_VERSION: u32 = 1;

/// Platform file name shared by every `libretro-database` category.
const PLATFORM_FILE: &str = "Nintendo - Super Nintendo Entertainment System.dat";
const LIBRETRO_DATABASE: &str =
    "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat";
const LIBRETRO_THUMBNAILS: &str = "https://raw.githubusercontent.com/libretro-thumbnails/Nintendo_-_Super_Nintendo_Entertainment_System/master/Named_Boxarts";
const WIKIPEDIA_SUMMARY: &str = "https://en.wikipedia.org/api/rest_v1/page/summary";

// --- CRC32 ----------------------------------------------------------------

/// CRC32 (IEEE, the one every DAT file uses) of a ROM image. Written out
/// rather than pulled in: it is twenty lines, and the alternative was a crate
/// in the dependency tree for a single polynomial.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// --- clrmamepro parsing ---------------------------------------------------

/// One `game ( … )` block: its own `key value` pairs, plus the pairs of every
/// block nested in it (`rom ( … )`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Block {
    pub fields: Vec<(String, String)>,
    pub nested: Vec<Vec<(String, String)>>,
}

impl Block {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// Every CRC this block names, in order. A DAT normally carries one `rom`
    /// per `game`, but nothing in the grammar says so.
    pub fn crcs(&self) -> Vec<u32> {
        self.nested
            .iter()
            .flat_map(|rom| rom.iter())
            .filter(|(k, _)| k == "crc")
            .filter_map(|(_, v)| u32::from_str_radix(v.trim(), 16).ok())
            .collect()
    }
}

/// Parse a clrmamepro file into its top-level blocks, as `(kind, block)` pairs
/// — `kind` is `clrmamepro`, `game`, `resource`…
///
/// These files are **not** XML, so there is no XML crate here. The grammar is
/// four tokens wide: a bare word, a `"quoted string"`, and the two parentheses.
/// A quoted value may hold anything, parentheses included (`name "Aero the
/// Acro-Bat (USA)"`), which is exactly why this is tokenized rather than split
/// on lines or on brackets.
///
/// A truncated file yields the blocks that did close, and drops the one that
/// did not: a half-downloaded catalogue must give fewer games, never wrong
/// ones.
pub fn parse_blocks(text: &str) -> Vec<(String, Block)> {
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        let Some(kind) = read_word(&bytes, &mut i) else { break };
        skip_space(&bytes, &mut i);
        if bytes.get(i) != Some(&'(') {
            // A stray word outside any block: skip it rather than resync on a
            // parenthesis that may belong to a quoted name.
            continue;
        }
        i += 1;
        match read_block(&bytes, &mut i) {
            Some(block) => out.push((kind, block)),
            None => break, // unterminated: the file is truncated
        }
    }
    out
}

/// Body of a block, up to and including its closing parenthesis. `None` when
/// the text ends first.
fn read_block(chars: &[char], i: &mut usize) -> Option<Block> {
    let mut block = Block::default();
    loop {
        skip_space(chars, i);
        match chars.get(*i) {
            None => return None,
            Some(')') => {
                *i += 1;
                return Some(block);
            }
            Some('(') => {
                // An anonymous nested block (never produced by these files);
                // consume it so the outer one still closes correctly.
                *i += 1;
                block.nested.push(read_block(chars, i)?.fields);
            }
            _ => {
                let key = read_word(chars, i)?;
                skip_space(chars, i);
                if chars.get(*i) == Some(&'(') {
                    *i += 1;
                    block.nested.push(read_block(chars, i)?.fields);
                } else {
                    let value = read_value(chars, i)?;
                    block.fields.push((key, value));
                }
            }
        }
    }
}

/// A bare word: everything up to whitespace or a parenthesis. `None` at the
/// end of the text.
fn read_word(chars: &[char], i: &mut usize) -> Option<String> {
    skip_space(chars, i);
    let start = *i;
    while let Some(c) = chars.get(*i) {
        if c.is_whitespace() || *c == '(' || *c == ')' || *c == '"' {
            break;
        }
        *i += 1;
    }
    (*i > start).then(|| chars[start..*i].iter().collect())
}

/// A value: a quoted string (which may hold parentheses and escaped quotes) or
/// a bare word (`users 2`, `crc 56410E5E`).
fn read_value(chars: &[char], i: &mut usize) -> Option<String> {
    skip_space(chars, i);
    if chars.get(*i) != Some(&'"') {
        return read_word(chars, i);
    }
    *i += 1;
    let mut out = String::new();
    while let Some(c) = chars.get(*i) {
        match c {
            '"' => {
                *i += 1;
                return Some(out);
            }
            '\\' => {
                *i += 1;
                if let Some(escaped) = chars.get(*i) {
                    out.push(*escaped);
                    *i += 1;
                }
            }
            other => {
                out.push(*other);
                *i += 1;
            }
        }
    }
    None // an unterminated string ends the file
}

fn skip_space(chars: &[char], i: &mut usize) {
    while chars.get(*i).is_some_and(|c| c.is_whitespace()) {
        *i += 1;
    }
}

/// CRC -> canonical name, from a No-Intro DAT. The first name wins when two
/// entries share a CRC (they never do in practice: the CRC *is* the identity
/// No-Intro is built on).
pub fn parse_no_intro(text: &str) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    for (kind, block) in parse_blocks(text) {
        if kind != "game" {
            continue;
        }
        let Some(name) = block.get("name") else { continue };
        for crc in block.crcs() {
            out.entry(crc).or_insert_with(|| name.to_string());
        }
    }
    out
}

/// CRC -> value of one category field (`genre`, `users`, `esrb_rating`…).
pub fn parse_category(text: &str, field: &str) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    for (kind, block) in parse_blocks(text) {
        if kind != "game" {
            continue;
        }
        let Some(value) = block.get(field).map(str::trim).filter(|v| !v.is_empty()) else {
            continue;
        };
        for crc in block.crcs() {
            out.entry(crc).or_insert_with(|| value.to_string());
        }
    }
    out
}

// --- the catalogue --------------------------------------------------------

/// One `libretro-database` category. `docs/METADATA.md` records how many SNES
/// entries each holds; the field name is not always the category name
/// (`maxusers` stores `users`, `esrb` stores `esrb_rating`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Genre,
    Developer,
    Publisher,
    MaxUsers,
    ReleaseYear,
    ReleaseMonth,
    Franchise,
    Esrb,
}

impl Category {
    pub const ALL: [Category; 8] = [
        Category::Genre,
        Category::Developer,
        Category::Publisher,
        Category::MaxUsers,
        Category::ReleaseYear,
        Category::ReleaseMonth,
        Category::Franchise,
        Category::Esrb,
    ];

    /// Directory of `metadat/`, and the local cache file's stem.
    pub fn dir(self) -> &'static str {
        match self {
            Category::Genre => "genre",
            Category::Developer => "developer",
            Category::Publisher => "publisher",
            Category::MaxUsers => "maxusers",
            Category::ReleaseYear => "releaseyear",
            Category::ReleaseMonth => "releasemonth",
            Category::Franchise => "franchise",
            Category::Esrb => "esrb",
        }
    }

    /// Key the value sits under inside a `game ( … )` block.
    pub fn field(self) -> &'static str {
        match self {
            Category::Genre => "genre",
            Category::Developer => "developer",
            Category::Publisher => "publisher",
            Category::MaxUsers => "users",
            Category::ReleaseYear => "releaseyear",
            Category::ReleaseMonth => "releasemonth",
            Category::Franchise => "franchise",
            Category::Esrb => "esrb_rating",
        }
    }
}

/// Name of the local cache file for a category, or for the No-Intro DAT.
fn catalog_file(dir: &str) -> String {
    format!("{dir}.dat")
}

fn catalog_url(dir: &str) -> String {
    format!("{LIBRETRO_DATABASE}/{}/{}", dir, net::encode_segment(PLATFORM_FILE))
}

/// The downloaded catalogue, parsed and held in memory by the library worker:
/// one CRC -> name map and one CRC -> value map per category. About four
/// thousand entries each, so a few megabytes for the whole set — paid once for
/// a library of any size.
#[derive(Debug, Default, Clone)]
pub struct Catalog {
    pub names: BTreeMap<u32, String>,
    pub facts: Vec<(Category, BTreeMap<u32, String>)>,
}

impl Catalog {
    /// Read the catalogue from `dir`, downloading through `net` whatever is not
    /// cached there yet and writing it down for next time.
    ///
    /// The No-Intro DAT failing is fatal — without it nothing downstream has a
    /// key. A *category* failing is not: the sheet then simply has one fact
    /// fewer, which is exactly what a game missing from that category looks
    /// like anyway.
    pub fn load(dir: &Path, net: &dyn Fetch) -> Result<Self, String> {
        let text = Self::file(dir, "no-intro", net)
            .map_err(|e| format!("No-Intro catalogue unavailable: {e}"))?;
        let names = parse_no_intro(&text);
        if names.is_empty() {
            return Err("No-Intro catalogue is empty or malformed".to_string());
        }
        let mut facts = Vec::with_capacity(Category::ALL.len());
        for category in Category::ALL {
            match Self::file(dir, category.dir(), net) {
                Ok(text) => facts.push((category, parse_category(&text, category.field()))),
                Err(e) => eprintln!("metadata: {} unavailable ({e}); skipped", category.dir()),
            }
        }
        Ok(Self { names, facts })
    }

    /// One catalogue file: from `dir` when it is already there, else fetched
    /// once and written down (atomically, like every other file this frontend
    /// persists).
    fn file(dir: &Path, name: &str, net: &dyn Fetch) -> Result<String, String> {
        let path = dir.join(catalog_file(name));
        if let Ok(text) = std::fs::read_to_string(&path) {
            if !text.is_empty() {
                return Ok(text);
            }
        }
        let bytes = net.get(&catalog_url(name)).map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if let Err(e) = crate::atomic::write(&path, text.as_bytes()) {
            // Not fatal: the catalogue is usable in memory, it will just be
            // downloaded again next run.
            eprintln!("metadata: could not cache {}: {e}", path.display());
        }
        Ok(text)
    }

    pub fn name_of(&self, crc: u32) -> Option<&str> {
        self.names.get(&crc).map(String::as_str)
    }

    pub fn fact(&self, category: Category, crc: u32) -> Option<&str> {
        self.facts
            .iter()
            .find(|(c, _)| *c == category)
            .and_then(|(_, map)| map.get(&crc))
            .map(String::as_str)
    }
}

// --- what one game ends up with -------------------------------------------

/// A description and where it came from. The three fields travel together on
/// purpose: the text is never shown without the article it was taken from.
///
/// Wikipedia is queried **by title**, so this is the one link of the chain
/// where a game homonymous with a film can win. Naming the article — and
/// linking it — is what makes a wrong match legible as a wrong match instead
/// of an assertion by the application. It is also the licence condition
/// (CC BY-SA), not a courtesy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Description {
    pub text: String,
    /// Title of the article actually served, which is not always the one asked
    /// for: the API follows redirects (`Super Mario RPG - Legend of the Seven
    /// Stars` lands on `Super Mario RPG`).
    pub title: String,
    pub url: String,
}

/// Everything the chain found for one game. Absent fields are absent, never
/// blank: `docs/METADATA.md` records that `releaseyear` covers 3305 of the
/// 3851 games and `esrb` only 1156, so a sheet with holes in it is the normal
/// case and must read as one.
///
/// **Difficulty is deliberately not here.** No open source carries it; it will
/// come later from an AI estimate, and will be labelled as an estimate rather
/// than lined up with these.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameMeta {
    /// CRC32 of the de-headered image these facts were looked up with. Stored
    /// so a replaced ROM invalidates the entry the same way `library.json`
    /// invalidates on `(size, mtime)`.
    pub crc32: u32,
    /// Canonical No-Intro name; empty when the CRC matched nothing.
    pub name: String,
    pub genre: Option<String>,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    /// Highest supported player count (`maxusers`).
    pub players: Option<String>,
    pub year: Option<String>,
    /// Month number, 1..=12, as the DAT writes it.
    pub month: Option<String>,
    pub franchise: Option<String>,
    pub esrb: Option<String>,
    pub description: Option<Description>,
    /// Box art PNG under `Boxarts/`; `None` when the repository had none.
    pub boxart: Option<PathBuf>,
    /// When this was filled in, Unix seconds.
    pub fetched: i64,
}

impl GameMeta {
    /// Whether the CRC was found in the No-Intro catalogue. False is a
    /// legitimate outcome — a translated, trainered or otherwise modified dump
    /// is simply not in it — and the sheet says so rather than looking broken.
    pub fn matched(&self) -> bool {
        !self.name.is_empty()
    }

}

/// The catalogue facts of one game, in display order, as label/value pairs —
/// the same shape `ui::game_sheet::facts` produces for the header facts, so
/// the two lists render through one code path and line up in one column.
pub fn facts(lang: Lang, meta: &GameMeta) -> Vec<(String, String)> {
    let rows: [(Msg, Option<String>); 7] = [
        (Msg::FactGenre, meta.genre.clone()),
        (Msg::FactDeveloper, meta.developer.clone()),
        (Msg::FactPublisher, meta.publisher.clone()),
        (Msg::FactPlayers, meta.players.clone()),
        (Msg::FactRelease, format_release(lang, meta.year.as_deref(), meta.month.as_deref())),
        (Msg::FactFranchise, meta.franchise.clone()),
        (Msg::FactEsrb, meta.esrb.clone()),
    ];
    rows.into_iter()
        .filter_map(|(msg, value)| value.map(|v| (msg.text(lang).to_string(), v)))
        .collect()
}

/// The release date, in each language's own order — `01/1993` reads as the
/// first of an unnamed month in English, `1993-01` reads as nothing at all in
/// French. The year alone when no month is catalogued, which is the case for
/// several hundred games.
pub fn format_release(lang: Lang, year: Option<&str>, month: Option<&str>) -> Option<String> {
    let year = year?.trim();
    if year.is_empty() {
        return None;
    }
    let month = month.and_then(|m| m.trim().parse::<u32>().ok()).filter(|m| (1..=12).contains(m));
    Some(match (lang, month) {
        (_, None) => year.to_string(),
        (Lang::Fr, Some(m)) => format!("{m:02}/{year}"),
        (Lang::En, Some(m)) => format!("{year}-{m:02}"),
    })
}

// --- Wikipedia ------------------------------------------------------------

/// Titles to try on Wikipedia for a canonical No-Intro name, best first.
///
/// Three rewrites, each of which fixes a known difference between the two
/// naming schemes, and no search step at all — a search would put the
/// resemblance the CRC removed back into the chain:
///
/// 1. the name with its parenthesised tags dropped (`(Europe)`, `(En,Fr,De)`,
///    `(Rev 1)`), which is right for the large majority;
/// 2. `A - B` rewritten `A: B`: No-Intro cannot use a colon in a file name and
///    Wikipedia does (`Super Mario World 2 - Yoshi's Island`);
/// 3. for a compilation, whose No-Intro name joins two titles with `and`, the
///    first title alone (`Super Mario All-Stars and Super Mario World` ->
///    `Super Mario All-Stars`) — Wikipedia has an article per game, not per
///    cartridge. Last, because it is the one rewrite that can shorten a real
///    title.
pub fn wiki_titles(canonical: &str) -> Vec<String> {
    let mut base = String::new();
    let mut depth = 0usize;
    for c in canonical.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            other if depth == 0 => base.push(other),
            _ => {}
        }
    }
    let base = base.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = Vec::new();
    let mut push = |title: String| {
        if !title.is_empty() && !out.contains(&title) {
            out.push(title);
        }
    };
    push(base.clone());
    push(base.replace(" - ", ": "));
    if let Some((first, _)) = base.split_once(" and ") {
        push(first.trim().to_string());
    }
    out
}

/// URL of the summary endpoint for one title.
pub fn wiki_url(title: &str) -> String {
    format!("{WIKIPEDIA_SUMMARY}/{}", net::encode_segment(&title.replace(' ', "_")))
}

/// Read a summary response. `None` for everything that is not a plain article
/// with prose in it: a **disambiguation** page (`Mercury` — a list of links,
/// never a description), a page with no extract, and any body that is not the
/// JSON object this endpoint documents.
pub fn parse_summary(body: &str) -> Option<Description> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let kind = value.get("type").and_then(|v| v.as_str()).unwrap_or("standard");
    if kind != "standard" {
        return None;
    }
    let text = value.get("extract").and_then(|v| v.as_str())?.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let title = value
        .get("titles")
        .and_then(|t| t.get("normalized"))
        .and_then(|v| v.as_str())
        .or_else(|| value.get("title").and_then(|v| v.as_str()))?
        .to_string();
    let url = value
        .get("content_urls")
        .and_then(|u| u.get("desktop"))
        .and_then(|d| d.get("page"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://en.wikipedia.org/wiki/{}", net::encode_segment(&title.replace(' ', "_"))));
    Some(Description { text, title, url })
}

// --- box art --------------------------------------------------------------

/// File name a box art carries in `libretro-thumbnails`: the canonical name
/// with the characters no filesystem shares (`&*/:\`<>?\|`) replaced by `_`,
/// which is the rule that repository is built with.
pub fn boxart_name(canonical: &str) -> String {
    canonical
        .chars()
        .map(|c| if "&*/:`<>?\\|\"".contains(c) { '_' } else { c })
        .collect()
}

pub fn boxart_url(canonical: &str) -> String {
    format!(
        "{LIBRETRO_THUMBNAILS}/{}",
        net::encode_segment(&format!("{}.png", boxart_name(canonical)))
    )
}

/// Where a game's box art is kept once downloaded. Keyed by `library::game_id`
/// rather than by the canonical name, like the generated thumbnails, so the
/// file is found without re-reading the catalogue.
pub fn boxart_path(id: &str) -> Option<PathBuf> {
    crate::prefs::data_path(BOXART_DIR).map(|d| d.join(format!("{id}.png")))
}

// --- the on-disk cache ----------------------------------------------------

/// Everything fetched so far, keyed by `library::game_id`.
///
/// **Not in `prefs.json`.** That file is what the player chose — volume,
/// bindings, folders — and losing it loses their work; this is derived data
/// that a button rebuilds from public servers, and it can be deleted at any
/// time with nothing lost but a few seconds. Keeping the two apart is the same
/// distinction `library.json` and `Thumbnails/` already draw, so it lives
/// beside them, under the same rule: a file with a foreign `version` is
/// discarded whole rather than migrated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub version: u32,
    pub games: BTreeMap<String, GameMeta>,
}

impl Default for Store {
    fn default() -> Self {
        Self { version: CACHE_VERSION, games: BTreeMap::new() }
    }
}

impl Store {
    pub fn read_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(store) if store.version == CACHE_VERSION => store,
            Ok(_) => Self::default(),
            Err(e) => {
                eprintln!("metadata: ignoring malformed {}: {e}", path.display());
                Self::default()
            }
        }
    }

    pub fn write_to(&self, path: &Path) -> Result<(), String> {
        let mut json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("could not serialize the metadata cache: {e}"))?;
        json.push('\n');
        crate::atomic::write(path, json.as_bytes())
    }

    /// The entry for `id`, if it was filled in for the ROM image now on disk.
    /// A different CRC means the file was replaced by another dump, and the
    /// old facts are not this game's.
    pub fn get(&self, id: &str, crc: u32) -> Option<&GameMeta> {
        self.games.get(id).filter(|m| m.crc32 == crc)
    }
}

/// Full path of `metadata.json`, or `None` with no config directory.
pub fn store_path() -> Option<PathBuf> {
    crate::prefs::data_path(CACHE_FILE)
}

/// Directory the catalogue files are cached in.
pub fn catalog_dir() -> Option<PathBuf> {
    crate::prefs::data_path(CATALOG_DIR)
}

// --- the fetch itself -----------------------------------------------------

/// Fill in one game: identify it by CRC, read its facts out of the catalogue
/// already in memory, then make at most one Wikipedia call and one box-art
/// call.
///
/// Always returns a `GameMeta`, even for a dump that is in no catalogue: the
/// answer "this dump is not in No-Intro" is worth recording and worth showing,
/// and re-asking the network for it on every visit to the sheet would not be.
/// Nothing here can panic and nothing here can block: the two network steps are
/// optional and their failure is the absence of a field.
pub fn fill(catalog: &Catalog, net: &dyn Fetch, id: &str, crc: u32) -> GameMeta {
    let mut meta = GameMeta { crc32: crc, fetched: crate::library::now_unix(), ..Default::default() };
    let Some(name) = catalog.name_of(crc).map(str::to_string) else {
        return meta;
    };
    meta.genre = catalog.fact(Category::Genre, crc).map(str::to_string);
    meta.developer = catalog.fact(Category::Developer, crc).map(str::to_string);
    meta.publisher = catalog.fact(Category::Publisher, crc).map(str::to_string);
    meta.players = catalog.fact(Category::MaxUsers, crc).map(str::to_string);
    meta.year = catalog.fact(Category::ReleaseYear, crc).map(str::to_string);
    meta.month = catalog.fact(Category::ReleaseMonth, crc).map(str::to_string);
    meta.franchise = catalog.fact(Category::Franchise, crc).map(str::to_string);
    meta.esrb = catalog.fact(Category::Esrb, crc).map(str::to_string);
    meta.description = fetch_description(net, &name);
    meta.boxart = fetch_boxart(net, id, &name);
    meta.name = name;
    meta
}

/// Walk `wiki_titles` until one answers with an article. A 404 moves on to the
/// next candidate; anything else stops the walk — a server that is refusing
/// requests must not be asked three times per game.
fn fetch_description(net: &dyn Fetch, canonical: &str) -> Option<Description> {
    for title in wiki_titles(canonical) {
        match net.get(&wiki_url(&title)) {
            Ok(body) => {
                if let Some(found) = parse_summary(&String::from_utf8_lossy(&body)) {
                    return Some(found);
                }
            }
            Err(net::Error::NotFound) => continue,
            Err(e) => {
                eprintln!("metadata: wikipedia {title:?}: {e}");
                return None;
            }
        }
    }
    None
}

/// Download the box art and write it beside the generated thumbnails. An
/// already-downloaded file is kept as is: the artwork does not change, and a
/// second "fill in everything" must not re-fetch four thousand pictures.
fn fetch_boxart(net: &dyn Fetch, id: &str, canonical: &str) -> Option<PathBuf> {
    let path = boxart_path(id)?;
    if path.is_file() {
        return Some(path);
    }
    match net.get(&boxart_url(canonical)) {
        Ok(bytes) => match crate::atomic::write(&path, &bytes) {
            Ok(()) => Some(path),
            Err(e) => {
                eprintln!("metadata: {e}");
                None
            }
        },
        Err(net::Error::NotFound) => None,
        Err(e) => {
            eprintln!("metadata: box art {canonical:?}: {e}");
            None
        }
    }
}

/// Offline stand-ins for the network, shared by this module's tests and by
/// `library`'s (which has to prove that a fetch that fails changes nothing).
#[cfg(test)]
pub mod testing {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A `net::Fetch` over a table of canned responses, so the whole chain runs
    /// offline. Records what was asked for, which is how the "one call per
    /// game, no retry storm" rules are checked.
    #[derive(Default)]
    pub struct FakeNet {
        answers: HashMap<String, Result<Vec<u8>, net::Error>>,
        pub asked: RefCell<Vec<String>>,
    }

    impl FakeNet {
        pub fn with(mut self, url: &str, body: &str) -> Self {
            self.answers.insert(url.to_string(), Ok(body.as_bytes().to_vec()));
            self
        }
        pub fn failing(mut self, url: &str) -> Self {
            self.answers.insert(url.to_string(), Err(net::Error::Failed("no route".into())));
            self
        }
        /// A server that refuses everything, whatever is asked of it.
        pub fn dead() -> Self {
            Self::default().failing(&catalog_url("no-intro"))
        }
    }

    impl Fetch for FakeNet {
        fn get(&self, url: &str) -> Result<Vec<u8>, net::Error> {
            self.asked.borrow_mut().push(url.to_string());
            self.answers.get(url).cloned().unwrap_or(Err(net::Error::NotFound))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeNet;
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("prisme_meta_{}_{}", std::process::id(), tag))
    }

    const NO_INTRO: &str = "clrmamepro (\n\tname \"Nintendo - Super Nintendo Entertainment System\"\n)\n\ngame (\n\tname \"Super Mario Kart (Europe)\"\n\tregion \"Europe\"\n\trom ( name \"Super Mario Kart (Europe).sfc\" size 524288 crc 56410E5E md5 F9FE md5 00 sha1 27D9 )\n)\ngame (\n\tname \"Aero the Acro-Bat (USA) (Beta)\"\n\trom ( name \"Aero the Acro-Bat (USA) (Beta).sfc\" size 1048576 crc 0BADC0DE )\n)\n";

    const GENRE: &str = "clrmamepro (\n\tname \"x\"\n)\n\ngame (\n\tcomment \"Super Mario Kart (Europe)\"\n\tgenre \"Racing\"\n\trom ( crc 56410E5E )\n)\n";

    #[test]
    fn the_crc_is_the_one_every_dat_is_keyed_on() {
        // The IEEE check value: "123456789" -> 0xCBF43926.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(&[0u8; 32]), 0x190A_55AD);
        // Order matters, so two different dumps of the same length differ.
        assert_ne!(crc32(b"ab"), crc32(b"ba"));
    }

    #[test]
    fn a_quoted_name_may_hold_the_parentheses_the_grammar_uses() {
        let names = parse_no_intro(NO_INTRO);
        assert_eq!(names.get(&0x5641_0E5E).map(String::as_str), Some("Super Mario Kart (Europe)"));
        // Two levels of parentheses inside one quoted value, which is what a
        // bracket-counting parser gets wrong.
        assert_eq!(
            names.get(&0x0BAD_C0DE).map(String::as_str),
            Some("Aero the Acro-Bat (USA) (Beta)")
        );
        assert_eq!(names.len(), 2, "the clrmamepro header is not a game");
    }

    #[test]
    fn a_category_is_keyed_by_crc_not_by_name() {
        // The design expected a name key; the files carry the CRC, with the
        // name only as a comment. This is what that costs to read.
        let genre = parse_category(GENRE, "genre");
        assert_eq!(genre.get(&0x5641_0E5E).map(String::as_str), Some("Racing"));
        assert!(genre.get(&0x0BAD_C0DE).is_none());
        // A bare (unquoted) value, the shape `maxusers` uses.
        let users = parse_category(
            "game (\n\tcomment \"A\"\n\tusers 3\n\trom ( crc 000000FF )\n)\n",
            "users",
        );
        assert_eq!(users.get(&0xFF).map(String::as_str), Some("3"));
        // A field this file does not carry yields nothing rather than blanks.
        assert!(parse_category(GENRE, "developer").is_empty());
    }

    #[test]
    fn a_truncated_file_yields_the_blocks_that_closed_and_no_others() {
        let cut = &NO_INTRO[..NO_INTRO.len() - 60];
        let names = parse_no_intro(cut);
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(names.contains_key(&0x5641_0E5E));
        // Cut in the middle of a quoted string, the worst place to stop.
        let names = parse_no_intro("game (\n\tname \"Super Mario Kart (Eur");
        assert!(names.is_empty(), "{names:?}");
        // And a file that is not a DAT at all.
        assert!(parse_no_intro("<xml><game/></xml>").is_empty());
        assert!(parse_no_intro("").is_empty());
    }

    #[test]
    fn a_crc_that_matches_nothing_yields_an_unmatched_entry_not_an_error() {
        let catalog = Catalog {
            names: parse_no_intro(NO_INTRO),
            facts: vec![(Category::Genre, parse_category(GENRE, "genre"))],
        };
        let net = FakeNet::default();
        let meta = fill(&catalog, &net, "GAME-0001", 0xDEAD_BEEF);
        assert!(!meta.matched());
        assert_eq!(meta.crc32, 0xDEAD_BEEF);
        assert!(meta.name.is_empty());
        assert!(facts(Lang::Fr, &meta).is_empty());
        assert_eq!(meta.description, None);
        // Nothing downstream was even asked: with no canonical name there is
        // no title to query and no picture to name.
        assert!(net.asked.borrow().is_empty(), "{:?}", net.asked.borrow());
    }

    #[test]
    fn every_category_has_its_own_directory_and_its_own_field() {
        let mut dirs: Vec<&str> = Category::ALL.iter().map(|c| c.dir()).collect();
        let count = dirs.len();
        dirs.sort_unstable();
        dirs.dedup();
        assert_eq!(dirs.len(), count, "two categories share a directory");
        // The two that do not name their own field, which is the whole reason
        // this mapping is explicit.
        assert_eq!(Category::MaxUsers.field(), "users");
        assert_eq!(Category::Esrb.field(), "esrb_rating");
        // The URLs are the documented ones, with the platform file encoded.
        assert_eq!(
            catalog_url("no-intro"),
            "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro/Nintendo%20-%20Super%20Nintendo%20Entertainment%20System.dat"
        );
    }

    #[test]
    fn a_catalogue_file_is_downloaded_once_and_read_from_disk_after_that() {
        let dir = scratch("catalog");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let net = FakeNet::default()
            .with(&catalog_url("no-intro"), NO_INTRO)
            .with(&catalog_url("genre"), GENRE);
        let catalog = Catalog::load(&dir, &net).expect("load");
        assert_eq!(catalog.name_of(0x5641_0E5E), Some("Super Mario Kart (Europe)"));
        assert_eq!(catalog.fact(Category::Genre, 0x5641_0E5E), Some("Racing"));
        // A category the server does not have is simply missing, not fatal.
        assert_eq!(catalog.fact(Category::Esrb, 0x5641_0E5E), None);
        let asked = net.asked.borrow().len();
        assert_eq!(asked, 1 + Category::ALL.len());

        // Second pass: everything that answered is now on disk, so only the
        // ones that failed are asked for again.
        let again = Catalog::load(&dir, &net).expect("reload");
        assert_eq!(again.name_of(0x5641_0E5E), Some("Super Mario Kart (Europe)"));
        assert_eq!(net.asked.borrow().len(), asked + Category::ALL.len() - 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_no_intro_catalogue_failing_is_the_only_fatal_step() {
        let dir = scratch("catalog-fail");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let net = FakeNet::default().failing(&catalog_url("no-intro"));
        assert!(Catalog::load(&dir, &net).is_err());
        // …and a DAT that answers with something that is not one.
        let net = FakeNet::default().with(&catalog_url("no-intro"), "<html>404</html>");
        assert!(Catalog::load(&dir, &net).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wikipedia_title_is_derived_from_the_canonical_name_without_searching() {
        assert_eq!(wiki_titles("Super Mario Kart (Europe)"), vec!["Super Mario Kart"]);
        assert_eq!(
            wiki_titles("Super Mario World 2 - Yoshi's Island (Europe) (En,Fr,De) (Rev 1)"),
            vec!["Super Mario World 2 - Yoshi's Island", "Super Mario World 2: Yoshi's Island"]
        );
        assert_eq!(
            wiki_titles("Super Mario All-Stars and Super Mario World (Europe)"),
            vec!["Super Mario All-Stars and Super Mario World", "Super Mario All-Stars"]
        );
        assert!(wiki_titles("(Europe)").is_empty());
        assert_eq!(
            wiki_url("Super Mario World 2: Yoshi's Island"),
            "https://en.wikipedia.org/api/rest_v1/page/summary/Super_Mario_World_2%3A_Yoshi%27s_Island"
        );
    }

    #[test]
    fn a_disambiguation_page_is_not_a_description() {
        let disambiguation = r#"{"type":"disambiguation","title":"Mercury","extract":"Mercury may refer to:","titles":{"normalized":"Mercury"}}"#;
        assert_eq!(parse_summary(disambiguation), None);
        // A 404 body, an empty extract, and anything that is not JSON.
        assert_eq!(parse_summary(r#"{"type":"https://mediawiki.org/wiki/HyperSwitch/errors/not_found","title":"Not found."}"#), None);
        assert_eq!(parse_summary(r#"{"type":"standard","title":"X","extract":"   "}"#), None);
        assert_eq!(parse_summary("<html>"), None);
        assert_eq!(parse_summary(""), None);

        let good = r#"{"type":"standard","title":"Super Mario RPG","titles":{"normalized":"Super Mario RPG"},"extract":"Super Mario RPG: Legend of the Seven Stars is a 1996 role-playing video game.","content_urls":{"desktop":{"page":"https://en.wikipedia.org/wiki/Super_Mario_RPG"}}}"#;
        let found = parse_summary(good).expect("a standard article is a description");
        assert_eq!(found.title, "Super Mario RPG");
        assert_eq!(found.url, "https://en.wikipedia.org/wiki/Super_Mario_RPG");
        assert!(found.text.starts_with("Super Mario RPG:"));
    }

    /// The redirect case: the title asked for is not the title served, and the
    /// sheet must credit the one that answered.
    #[test]
    fn the_article_that_answered_is_the_one_credited() {
        let body = r#"{"type":"standard","title":"Yoshi's Island","titles":{"normalized":"Yoshi's Island"},"extract":"Yoshi's Island is a 1995 platform game.","content_urls":{"desktop":{"page":"https://en.wikipedia.org/wiki/Yoshi%27s_Island"}}}"#;
        let net = FakeNet::default()
            .with(&wiki_url("Super Mario World 2: Yoshi's Island"), body);
        let found = fetch_description(&net, "Super Mario World 2 - Yoshi's Island (Europe)")
            .expect("the second candidate answers");
        assert_eq!(found.title, "Yoshi's Island");
        // The first candidate was tried and 404'd, the second answered, and no
        // third call was made.
        assert_eq!(net.asked.borrow().len(), 2);
    }

    /// A server that is refusing requests must be asked once per game, not
    /// once per candidate title.
    #[test]
    fn a_refusing_server_ends_the_walk_instead_of_being_hammered() {
        let net = FakeNet::default().failing(&wiki_url("Super Mario World 2 - Yoshi's Island"));
        assert_eq!(fetch_description(&net, "Super Mario World 2 - Yoshi's Island (Europe)"), None);
        assert_eq!(net.asked.borrow().len(), 1);
    }

    #[test]
    fn the_box_art_file_name_is_the_canonical_name_the_repository_uses() {
        assert_eq!(boxart_name("Super Mario Kart (Europe)"), "Super Mario Kart (Europe)");
        // The characters that repository replaces, and only those.
        assert_eq!(boxart_name("Jam & Jelly: A/B?"), "Jam _ Jelly_ A_B_");
        assert_eq!(boxart_name("Yoshi's Island (En,Fr,De)"), "Yoshi's Island (En,Fr,De)");
        assert_eq!(
            boxart_url("Super Mario Kart (Europe)"),
            "https://raw.githubusercontent.com/libretro-thumbnails/Nintendo_-_Super_Nintendo_Entertainment_System/master/Named_Boxarts/Super%20Mario%20Kart%20%28Europe%29.png"
        );
    }

    #[test]
    fn a_release_date_is_written_the_way_each_language_reads_it() {
        assert_eq!(format_release(Lang::Fr, Some("1993"), Some("1")), Some("01/1993".into()));
        assert_eq!(format_release(Lang::En, Some("1993"), Some("1")), Some("1993-01".into()));
        // A year with no month is the common case (546 SNES games have one and
        // not the other) and must not print a fake month.
        assert_eq!(format_release(Lang::Fr, Some("1996"), None), Some("1996".into()));
        assert_eq!(format_release(Lang::Fr, Some("1996"), Some("13")), Some("1996".into()));
        assert_eq!(format_release(Lang::Fr, None, Some("7")), None);
        assert_eq!(format_release(Lang::Fr, Some(" "), None), None);
    }

    #[test]
    fn the_catalogue_facts_are_listed_in_both_languages_and_skip_what_is_absent() {
        let meta = GameMeta {
            name: "Secret of Mana (France)".into(),
            genre: Some("Role-playing (RPG)".into()),
            developer: Some("Square".into()),
            players: Some("3".into()),
            year: Some("1994".into()),
            month: Some("11".into()),
            ..Default::default()
        };
        let fr: BTreeMap<_, _> = facts(Lang::Fr, &meta).into_iter().collect();
        assert_eq!(fr["Genre"], "Role-playing (RPG)");
        assert_eq!(fr["Développeur"], "Square");
        assert_eq!(fr["Joueurs"], "3");
        assert_eq!(fr["Sortie"], "11/1994");
        // An absent fact is an absent row, never an empty one.
        assert!(!fr.contains_key("Éditeur"));
        assert!(!fr.contains_key("Classification"));
        let en: BTreeMap<_, _> = facts(Lang::En, &meta).into_iter().collect();
        assert_eq!(en["Developer"], "Square");
        assert_eq!(en["Players"], "3");
        assert_eq!(en["Release"], "1994-11");
        assert!(facts(Lang::Fr, &GameMeta::default()).is_empty());
    }

    #[test]
    fn the_store_round_trips_and_a_replaced_dump_invalidates_its_entry() {
        let dir = scratch("store");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(CACHE_FILE);
        let mut store = Store::default();
        let meta = GameMeta {
            crc32: 0x5641_0E5E,
            name: "Super Mario Kart (Europe)".into(),
            genre: Some("Racing".into()),
            description: Some(Description {
                text: "Super Mario Kart is a 1992 kart racing game.".into(),
                title: "Super Mario Kart".into(),
                url: "https://en.wikipedia.org/wiki/Super_Mario_Kart".into(),
            }),
            boxart: Some(PathBuf::from("/box/SUPER_MARIOKART-BEEF.png")),
            fetched: 1_768_478_400,
            ..Default::default()
        };
        store.games.insert("SUPER_MARIOKART-BEEF".into(), meta.clone());
        store.write_to(&path).expect("write");
        let back = Store::read_from(&path);
        assert_eq!(back, store);
        assert_eq!(back.get("SUPER_MARIOKART-BEEF", 0x5641_0E5E), Some(&meta));
        // Same game id, different dump: the facts are not this file's.
        assert_eq!(back.get("SUPER_MARIOKART-BEEF", 0x0000_0001), None);
        assert_eq!(back.get("UNKNOWN-0000", 0x5641_0E5E), None);

        // Derived data: a foreign version and a corrupt file are both dropped
        // whole rather than half-read.
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("\"version\": 1", "\"version\": 99")).expect("rewrite");
        assert_eq!(Store::read_from(&path), Store::default());
        std::fs::write(&path, b"{ not json").expect("rewrite");
        assert_eq!(Store::read_from(&path), Store::default());
        assert_eq!(Store::read_from(&dir.join("absent.json")), Store::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole chain, offline, on the shape of data the real servers return.
    #[test]
    fn the_whole_chain_runs_from_a_crc_to_a_described_game() {
        let dir = scratch("chain");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let summary = r#"{"type":"standard","title":"Super Mario Kart","titles":{"normalized":"Super Mario Kart"},"extract":"Super Mario Kart is a 1992 kart racing game developed and published by Nintendo.","content_urls":{"desktop":{"page":"https://en.wikipedia.org/wiki/Super_Mario_Kart"}}}"#;
        let net = FakeNet::default()
            .with(&catalog_url("no-intro"), NO_INTRO)
            .with(&catalog_url("genre"), GENRE)
            .with(&wiki_url("Super Mario Kart"), summary);
        let catalog = Catalog::load(&dir, &net).expect("catalogue");
        let meta = fill(&catalog, &net, "SUPER_MARIOKART-BEEF", 0x5641_0E5E);
        assert!(meta.matched());
        assert_eq!(meta.name, "Super Mario Kart (Europe)");
        assert_eq!(meta.genre.as_deref(), Some("Racing"));
        let description = meta.description.expect("a description");
        assert_eq!(description.title, "Super Mario Kart");
        assert!(description.url.contains("wikipedia.org"));
        // No box art was served: the field is absent, and that is all.
        assert_eq!(meta.boxart, None);
        assert!(meta.fetched > 1_700_000_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole chain against the real servers, on the project's own ROM
    /// folder — the only way to know that the CRCs this code computes are the
    /// CRCs No-Intro is keyed on. Ignored by default: it needs the network and
    /// the ROMs, neither of which is part of the source. Run with
    /// `cargo test -p prisme --release -- --ignored --nocapture the_real_rom_folder`.
    #[test]
    #[ignore]
    fn the_real_rom_folder_is_identified_by_its_fingerprints() {
        let roms = Path::new("../roms");
        let roms = if roms.is_dir() { roms } else { Path::new("roms") };
        assert!(roms.is_dir(), "ROM folder missing: {}", roms.display());
        let dir = catalog_dir().expect("a config directory");
        let net = crate::net::Http::new();
        let catalog = Catalog::load(&dir, &net).expect("catalogue");
        eprintln!("catalogue: {} No-Intro entries", catalog.names.len());

        let mut cache = crate::library::Cache::default();
        let entries = crate::library::scan(roms, &[], &mut cache).expect("scan");
        assert!(!entries.is_empty());
        let mut matched = 0;
        for entry in &entries {
            let meta = fill(&catalog, &net, &entry.id, entry.crc32);
            matched += usize::from(meta.matched());
            eprintln!(
                "{:52} {:08X}  {}",
                entry.file_name(),
                entry.crc32,
                if meta.matched() { meta.name.as_str() } else { "— not in No-Intro —" }
            );
            eprintln!(
                "      facts: {:?}  description: {}  box art: {}",
                facts(Lang::En, &meta),
                meta.description
                    .as_ref()
                    .map(|d| format!("{} ({} chars)", d.title, d.text.chars().count()))
                    .unwrap_or_else(|| "none".to_string()),
                meta.boxart.is_some()
            );
        }
        // A modified dump legitimately misses; most of a real folder must not.
        assert!(matched * 2 > entries.len(), "{matched} of {} identified", entries.len());
    }

    /// A failed fetch must leave the entry that was there exactly as it was.
    /// This is the rule the whole feature is judged on, so it is asserted on
    /// the store itself, not only on the pieces.
    #[test]
    fn a_failing_network_never_touches_what_was_already_stored() {
        let dir = scratch("keeps");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(CACHE_FILE);
        let known = GameMeta {
            crc32: 0x5641_0E5E,
            name: "Super Mario Kart (Europe)".into(),
            genre: Some("Racing".into()),
            fetched: 1_768_478_400,
            ..Default::default()
        };
        let mut store = Store::default();
        store.games.insert("SUPER_MARIOKART-BEEF".into(), known.clone());
        store.write_to(&path).expect("write");

        // Everything refuses: no catalogue, so no fetch can even start.
        let net = FakeNet::default().failing(&catalog_url("no-intro"));
        assert!(Catalog::load(&dir, &net).is_err());
        assert_eq!(Store::read_from(&path).get("SUPER_MARIOKART-BEEF", 0x5641_0E5E), Some(&known));

        // And with a catalogue but no Wikipedia and no box art, the game is
        // still identified and simply has no prose.
        let net = FakeNet::default()
            .with(&catalog_url("no-intro"), NO_INTRO)
            .with(&catalog_url("genre"), GENRE)
            .failing(&wiki_url("Super Mario Kart"));
        let catalog = Catalog::load(&dir, &net).expect("catalogue");
        let meta = fill(&catalog, &net, "SUPER_MARIOKART-BEEF", 0x5641_0E5E);
        assert!(meta.matched());
        assert_eq!(meta.description, None);
        assert_eq!(meta.genre.as_deref(), Some("Racing"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
