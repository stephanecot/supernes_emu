//! French and English, side by side, with the compiler as the proof-reader.
//!
//! **Why a macro and not a translation file.** A `.po`/`.json` catalogue lets a
//! missing string through to the screen, where it shows up as a blank label or
//! a raw key — and only once someone runs the application in that language.
//! Here the two languages are written on the same line and `Msg::text` matches
//! exhaustively, so a variant added without its English simply does not
//! compile. The cost is a little ceremony; the gain is that no screen can ever
//! be half translated.
//!
//! **What is not here.** Command-line help and the diagnostic output
//! (`--trace`, `--log-mmio`, `--info`) stay English: their audience reads it
//! and their wording is an anchor for scripts. Game titles, chip names
//! (SuperFX, SA-1, DSP-1, CX4) and the pad's own letters (`A` `B` `X` `Y`
//! `L` `R` `Start` `Select`) are silk-screen, not prose, and are never
//! translated either.
//!
//! **Sentences with holes are functions**, not constants, and each language
//! gets its whole template with the holes where *it* wants them. Building a
//! sentence by concatenation produces English wearing a French coat.

use std::fmt;

/// The languages the interface speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    Fr,
    En,
}

impl Lang {
    /// Value stored in `prefs.json`. `system` is not a `Lang`: it is the
    /// *absence* of a choice, and lives in the preference as `None`.
    pub fn as_pref(self) -> &'static str {
        match self {
            Lang::Fr => "fr",
            Lang::En => "en",
        }
    }

    /// Parse a stored preference. Unknown text yields `None`, which the caller
    /// reads as "follow the system" — an unreadable value must never leave the
    /// interface blank.
    pub fn from_pref(s: &str) -> Option<Self> {
        match s {
            "fr" => Some(Lang::Fr),
            "en" => Some(Lang::En),
            _ => None,
        }
    }

    /// How this language names itself. Endonyms, deliberately: nobody looks for
    /// "Anglais" in order to switch to English.
    pub fn endonym(self) -> &'static str {
        match self {
            Lang::Fr => "Français",
            Lang::En => "English",
        }
    }

    /// Language matching an IETF-ish tag (`fr`, `fr-CA`, `fr_FR`, `en-GB`).
    /// Anything that is not French falls to English, which is the wider net of
    /// the two.
    pub fn from_tag(tag: &str) -> Self {
        let primary = tag
            .split(['-', '_', '.'])
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if primary == "fr" {
            Lang::Fr
        } else {
            Lang::En
        }
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_pref())
    }
}

/// Declare a message in both languages at once.
macro_rules! messages {
    ($($(#[$doc:meta])* $variant:ident => $fr:literal / $en:literal),* $(,)?) => {
        /// Every fixed string the interface shows.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Msg {
            $($(#[$doc])* $variant),*
        }

        impl Msg {
            pub fn text(self, lang: Lang) -> &'static str {
                match self {
                    $(Msg::$variant => match lang {
                        Lang::Fr => $fr,
                        Lang::En => $en,
                    }),*
                }
            }

            /// Every variant, walked by the tests. Test-only: the screens name
            /// the message they want.
            #[cfg(test)]
            const ALL: &'static [Msg] = &[$(Msg::$variant),*];
        }
    };
}

messages! {
    // --- Shell chrome ---------------------------------------------------
    AppTagline        => "Émulateur Super Nintendo" / "Super Nintendo emulator",
    Back              => "Retour" / "Back",
    Quit              => "Quitter" / "Quit",
    Cancel            => "Annuler" / "Cancel",
    Close             => "Fermer" / "Close",

    // --- Tabs -----------------------------------------------------------
    TabLibrary        => "Bibliothèque" / "Library",
    TabFavorites      => "Favoris" / "Favourites",
    TabRecent         => "Récents" / "Recent",
    TabSettings       => "Réglages" / "Settings",

    // --- Library --------------------------------------------------------
    SearchPlaceholder => "titre ou nom de fichier" / "title or file name",
    SortBy            => "Trier par" / "Sort by",
    SortTitle         => "Titre" / "Title",
    SortRecent        => "Récemment joué" / "Recently played",
    Refresh           => "Actualiser" / "Refresh",
    ChooseFolder      => "Dossier…" / "Folder…",
    AddGame           => "Ajouter un jeu…" / "Add a game…",
    AddGameHint       => "Ajouter un jeu situé hors du dossier — ou déposez son fichier sur la fenêtre"
                       / "Add a game from outside the folder — or drop its file on the window",
    Play              => "Jouer" / "Play",
    AddToFavorites    => "Ajouter aux favoris" / "Add to favourites",
    Favorite          => "Favori" / "Favourite",
    NoPicture         => "pas de miniature" / "no thumbnail",
    PictureRunning    => "miniature en cours…" / "building thumbnail…",
    ScanningFolder    => "Analyse du dossier…" / "Scanning the folder…",

    // --- A game whose file is gone ---------------------------------------
    FileMissing       => "Fichier introuvable" / "File not found",
    RelocateGame      => "Retrouver le fichier…" / "Locate the file…",
    RelocateHint      => "Le désigner à son nouvel emplacement, ou déposer le fichier sur la fenêtre"
                       / "Point at its new location, or drop the file on the window",
    ForgetGame        => "Retirer de la bibliothèque" / "Remove from the library",
    ForgetHint        => "Le fichier lui-même n'est pas supprimé" / "The file itself is not deleted",

    // --- Settings sections ------------------------------------------------
    SectionDisplay    => "Affichage" / "Display",
    SectionAudio      => "Audio" / "Audio",
    SectionEmulation  => "Émulation" / "Emulation",
    SectionInputs     => "Entrées" / "Controls",
    SectionFolders    => "Dossiers" / "Folders",
    SectionAbout      => "À propos" / "About",

    // --- Display ----------------------------------------------------------
    Language          => "Langue" / "Language",
    WindowSize        => "Taille de la fenêtre" / "Window size",
    NativeSize        => "Taille native (256 × 224)" / "Native size (256 × 224)",
    Filter            => "Filtre" / "Filter",
    FilterNone        => "Aucun" / "None",
    FilterSmooth      => "Lissé" / "Smooth",
    FilterCrt         => "CRT" / "CRT",
    Aspect            => "Ratio" / "Aspect",
    AspectPixel       => "Pixel-parfait (1:1)" / "Pixel-perfect (1:1)",
    AspectTv          => "TV authentique (8:7)" / "Authentic TV (8:7)",
    Fullscreen        => "Plein écran" / "Full screen",
    FrameCounter      => "Compteur d'images" / "Frame counter",

    // --- Controls ---------------------------------------------------------
    RebindHint        => "Cliquez sur une case — ou sur le bouton dessiné — pour la réaffecter."
                       / "Click a cell — or a button on the drawing — to reassign it.",
    ResetInputs       => "Rétablir les entrées par défaut" / "Restore the default controls",
    ColumnButton      => "Bouton" / "Button",
    ColumnKeyboard    => "Clavier" / "Keyboard",
    ColumnPad         => "Manette" / "Controller",
    PadArtHint        => "Les boutons pressés s'allument : de quoi vérifier une manette sans lancer de jeu."
                       / "Pressed buttons light up: a way to check a controller without starting a game.",

    // --- About ------------------------------------------------------------
    AboutBlurb        => "Émulateur Super Nintendo écrit en Rust : cœur d'émulation sans entrées/sorties, interface séparée."
                       / "A Super Nintendo emulator written in Rust: an I/O-free emulation core, with the interface kept apart.",
    Guide             => "Guide pédagogique" / "Learning guide",
    AppFiles          => "Fichiers de l'application" / "Application files",
    AppFilesHint      => "Préférences, cache de la bibliothèque et miniatures ; supprimables sans rien perdre."
                       / "Preferences, library cache and thumbnails; safe to delete, nothing is lost.",
}

/// What the footer says Escape does.
pub fn escape_hint(lang: Lang, has_session: bool) -> &'static str {
    match (lang, has_session) {
        (Lang::Fr, true) => "Échap : revenir au jeu · O : ouvrir une ROM · , : réglages",
        (Lang::Fr, false) => "O : ouvrir une ROM · , : réglages",
        (Lang::En, true) => "Esc: back to the game · O: open a ROM · ,: settings",
        (Lang::En, false) => "O: open a ROM · ,: settings",
    }
}

/// Waiting for a key or a pad button to bind to `button`.
///
/// Written out per language rather than assembled: French puts the button after
/// the verb phrase, English fronts it, and a shared template would force one of
/// them into the other's word order.
pub fn press_a_key_for(lang: Lang, button: &str) -> String {
    match lang {
        Lang::Fr => format!("Appuyez sur une touche pour {button} — Échap pour annuler."),
        Lang::En => format!("Press a key for {button} — Esc to cancel."),
    }
}

/// A ROM that could not be read, named by its file.
pub fn cannot_load(lang: Lang, name: &str, error: &str) -> String {
    match lang {
        Lang::Fr => format!("Impossible de charger {name} : {error}"),
        Lang::En => format!("Could not load {name}: {error}"),
    }
}

/// A file the player pointed at that is not a cartridge at all.
pub fn not_a_rom(lang: Lang, name: &str) -> String {
    match lang {
        Lang::Fr => format!("{name} n'est pas une ROM Super Nintendo (.sfc, .smc ou .zip)."),
        Lang::En => format!("{name} is not a Super Nintendo ROM (.sfc, .smc or .zip)."),
    }
}

/// Language the host is set to. macOS keeps the ordered list in
/// `AppleLanguages`; elsewhere the POSIX variables carry it. An unset or
/// unreadable environment falls to English rather than to nothing.
pub fn system_lang() -> Lang {
    #[cfg(target_os = "macos")]
    if let Some(tag) = macos_preferred_language() {
        return Lang::from_tag(&tag);
    }
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() && value != "C" && value != "POSIX" {
                return Lang::from_tag(&value);
            }
        }
    }
    Lang::En
}

/// First entry of the user's `AppleLanguages`, read through `defaults` rather
/// than by linking CoreFoundation: this is read once at startup, so a process
/// is cheaper than a framework dependency the rest of the shell does not need.
#[cfg(target_os = "macos")]
fn macos_preferred_language() -> Option<String> {
    let out = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    // `("fr-FR", "en-US")` over several lines; the first quoted tag wins.
    let start = text.find('"')? + 1;
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_exists_in_both_languages() {
        for msg in Msg::ALL {
            for lang in [Lang::Fr, Lang::En] {
                let text = msg.text(lang);
                assert!(!text.trim().is_empty(), "{msg:?} is empty in {lang}");
            }
        }
    }

    /// Not a style rule but a correctness one: two variants rendering the same
    /// French text are almost always a copy-paste that will be translated once
    /// and read wrong in the other place.
    #[test]
    fn no_two_messages_share_a_french_string() {
        let mut seen: Vec<(&str, Msg)> = Vec::new();
        for &msg in Msg::ALL {
            let text = msg.text(Lang::Fr);
            if let Some((_, first)) = seen.iter().find(|(t, _)| *t == text) {
                panic!("{msg:?} and {first:?} both render {text:?}");
            }
            seen.push((text, msg));
        }
    }

    #[test]
    fn a_language_tag_resolves_to_the_language_it_names() {
        for tag in ["fr", "fr-FR", "fr_CA", "FR", "fr-CA.UTF-8"] {
            assert_eq!(Lang::from_tag(tag), Lang::Fr, "{tag}");
        }
        // Everything else is English, including the empty and the absurd: an
        // unknown locale must never leave the interface blank.
        for tag in ["en", "en-GB", "de-DE", "ja", "", "zz", "C"] {
            assert_eq!(Lang::from_tag(tag), Lang::En, "{tag}");
        }
    }

    #[test]
    fn a_stored_language_round_trips_and_an_unknown_one_defers_to_the_system() {
        for lang in [Lang::Fr, Lang::En] {
            assert_eq!(Lang::from_pref(lang.as_pref()), Some(lang));
        }
        assert_eq!(Lang::from_pref("system"), None);
        assert_eq!(Lang::from_pref("klingon"), None);
        assert_eq!(Lang::from_pref(""), None);
    }

    #[test]
    fn each_language_names_itself_in_its_own_words() {
        assert_eq!(Lang::Fr.endonym(), "Français");
        assert_eq!(Lang::En.endonym(), "English");
    }

    #[test]
    fn a_sentence_with_a_hole_keeps_each_language_word_order() {
        assert_eq!(
            press_a_key_for(Lang::Fr, "A"),
            "Appuyez sur une touche pour A — Échap pour annuler."
        );
        assert_eq!(press_a_key_for(Lang::En, "A"), "Press a key for A — Esc to cancel.");
        assert!(not_a_rom(Lang::En, "notes.txt").starts_with("notes.txt is not"));
        assert!(not_a_rom(Lang::Fr, "notes.txt").starts_with("notes.txt n'est pas"));
    }
}
