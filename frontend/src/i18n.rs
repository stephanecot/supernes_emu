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
//! (SuperFX, SA-1, DSP-1, CX4), the pad's own letters (`A` `B` `X` `Y`
//! `L` `R` `Start` `Select`) and the names of keyboard keys (`Enter`,
//! `Right Shift`, `Arrow Up` — see `input::key_label`) are silk-screen, not
//! prose: they are what is printed on the hardware, and they stay English in
//! both languages.
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
    /// Both languages, in the order the selector offers them.
    pub const ALL: [Lang; 2] = [Lang::Fr, Lang::En];

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
    BackEsc           => "Retour (Échap)" / "Back (Esc)",
    Quit              => "Quitter" / "Quit",
    QuitEnter         => "Quitter (Entrée)" / "Quit (Enter)",
    Cancel            => "Annuler" / "Cancel",
    CancelEsc         => "Annuler (Échap)" / "Cancel (Esc)",
    OpenRom           => "Ouvrir une ROM…" / "Open a ROM…",
    QuitPromise       => "La sauvegarde de la cartouche et l'état de session seront écrits avant de quitter."
                       / "The cartridge save and the session state will be written before quitting.",

    // --- Tabs -----------------------------------------------------------
    TabLibrary        => "Bibliothèque" / "Library",
    TabFavorites      => "Favoris" / "Favourites",
    TabRecent         => "Récents" / "Recent",
    TabSettings       => "Réglages" / "Settings",

    // --- Library --------------------------------------------------------
    SearchPlaceholder => "titre ou nom de fichier" / "title or file name",
    Clear             => "Effacer" / "Clear",
    ClearSearch       => "Effacer la recherche" / "Clear the search",
    SortBy            => "Trier par" / "Sort by",
    SortTitle         => "Titre" / "Title",
    SortRecent        => "Récemment joué" / "Recently played",
    Refresh           => "Actualiser" / "Refresh",
    ChooseFolder      => "Dossier…" / "Folder…",
    AddGame           => "Ajouter un jeu…" / "Add a game…",
    AddGameHint       => "Ajouter un jeu situé hors du dossier — ou déposez son fichier sur la fenêtre"
                       / "Add a game from outside the folder — or drop its file on the window",
    Play              => "Jouer" / "Play",
    Resume            => "Reprendre" / "Resume",
    StartOver         => "Nouvelle partie" / "New game",
    StartOverHint     => "Démarre la cartouche à zéro. La partie en cours est conservée et reste reprenable."
                       / "Starts the cartridge from scratch. The suspended session is kept and stays resumable.",
    AddToFavorites    => "Ajouter aux favoris" / "Add to favourites",
    Favorite          => "Favori" / "Favourite",
    NoPicture         => "pas de miniature" / "no thumbnail",
    PictureRunning    => "miniature en cours…" / "building thumbnail…",
    NoPreview         => "pas d'aperçu" / "no preview",
    ScanningFolder    => "Analyse du dossier…" / "Scanning the folder…",

    // --- Empty screens ----------------------------------------------------
    EmptyLibrary      => "Aucun jeu ici." / "No game here.",
    EmptyLibraryHint  => "Choisissez le dossier qui contient vos ROMs."
                       / "Choose the folder that holds your ROMs.",
    EmptySearch       => "Aucun jeu ne correspond à cette recherche."
                       / "No game matches this search.",
    EmptySearchHint   => "Essayez un autre mot, ou effacez la recherche."
                       / "Try another word, or clear the search.",
    EmptyFavorites    => "Aucun favori." / "No favourite yet.",
    EmptyFavoritesHint => "L'étoile d'une tuile épingle un jeu ici."
                       / "A tile's star pins a game here.",
    EmptyRecent       => "Aucune partie récente." / "Nothing played recently.",
    EmptyRecentHint   => "Lancez un jeu : il apparaîtra ici."
                       / "Start a game and it will show up here.",
    ChooseRomFolder   => "Choisir un dossier de ROMs…" / "Choose a ROM folder…",

    // --- A game whose file is gone ---------------------------------------
    FileMissing       => "Fichier introuvable" / "File not found",
    RelocateGame      => "Retrouver le fichier…" / "Locate the file…",
    RelocateHint      => "Le désigner à son nouvel emplacement, ou déposer le fichier sur la fenêtre"
                       / "Point at its new location, or drop the file on the window",
    ForgetGame        => "Retirer de la bibliothèque" / "Remove from the library",
    ForgetHint        => "Le fichier lui-même n'est pas supprimé" / "The file itself is not deleted",

    // --- Game sheet -------------------------------------------------------
    GeneratedThumbnail => "Vignette générée" / "Generated picture",
    GeneratedThumbnailHint => "Revenir à la miniature produite par l'émulateur"
                       / "Go back to the picture the emulator produced",
    SaveStates        => "Sauvegardes d'état" / "Save states",
    NoSaveStates      => "Aucune sauvegarde d'état pour ce jeu. F5 en enregistre une."
                       / "No save state for this game. F5 writes one.",
    Screenshots       => "Captures d'écran" / "Screenshots",
    NoScreenshots     => "Aucune capture. F12 pendant une partie en enregistre une."
                       / "No screenshot. F12 during a game takes one.",
    PromoteHint       => "Cliquez une capture pour en faire la vignette du jeu."
                       / "Click a screenshot to make it the game's picture.",
    ResumeState       => "Reprise" / "Resume",
    SavedWithoutPreview => "Sauvegardé sans aperçu" / "Saved with no picture",
    Delete            => "Supprimer…" / "Delete…",
    DeleteHint        => "Efface le fichier d'état et son aperçu"
                       / "Erases the state file and its picture",
    DeleteForever     => "Supprimer définitivement" / "Delete for good",
    CurrentThumbnail  => "Vignette actuelle du jeu" / "The game's current picture",
    UseAsThumbnail    => "Utiliser comme vignette" / "Use as the game's picture",

    // --- Cartridge facts --------------------------------------------------
    FactRegion        => "Région" / "Region",
    FactMapping       => "Mapping" / "Mapping",
    FactSize          => "Taille" / "Size",
    FactSave          => "Sauvegarde" / "Battery save",
    FactCoprocessor   => "Coprocesseur" / "Coprocessor",
    FactChecksum      => "Somme de contrôle" / "Checksum",
    FactPlayTime      => "Temps de jeu" / "Play time",
    FactLastPlayed    => "Dernière partie" / "Last played",
    ChecksumValid     => "valide" / "valid",
    ChecksumInvalid   => "INVALIDE" / "INVALID",
    /// A masculine "none": no filter, no coprocessor.
    NoneMasculine     => "Aucun" / "None",
    /// …and a feminine one: no battery save. English has one word for both.
    NoneFeminine      => "Aucune" / "None",
    NeverPlayed       => "Jamais joué" / "Never played",
    LessThanAMinute   => "moins d'une minute" / "less than a minute",

    // --- Settings sections ------------------------------------------------
    SectionDisplay    => "Affichage" / "Display",
    SectionAudio      => "Audio" / "Audio",
    SectionEmulation  => "Émulation" / "Emulation",
    SectionInputs     => "Entrées" / "Controls",
    SectionFolders    => "Dossiers" / "Folders",
    SectionAbout      => "À propos" / "About",
    SettingsFooter    => "Échap : revenir · chaque changement est enregistré aussitôt"
                       / "Esc: back · every change is saved at once",

    // --- Display ----------------------------------------------------------
    Language          => "Langue" / "Language",
    LanguageSystem    => "Système" / "System",
    WindowSize        => "Taille de la fenêtre" / "Window size",
    WindowSizeHint    => "La fenêtre reste librement redimensionnable ; ces paliers fixent une taille (F1-F4). Sans choix, elle s'ouvre à la plus grande taille confortable sur cet écran."
                       / "The window stays freely resizable; these steps only set a size (F1-F4). With no choice made, it opens at the largest comfortable size for this monitor.",
    Filter            => "Filtre" / "Filter",
    FilterSmooth      => "Lissé" / "Smooth",
    FilterCrt         => "CRT" / "CRT",
    Aspect            => "Ratio" / "Aspect",
    AspectPixel       => "Pixel-parfait (1:1)" / "Pixel-perfect (1:1)",
    AspectTv          => "TV authentique (8:7)" / "Authentic TV (8:7)",
    AspectHint        => "L'image n'est jamais déformée : bandes noires si la fenêtre ne tombe pas juste."
                       / "The picture is never stretched: black bars when the window does not fall right.",
    Fullscreen        => "Plein écran" / "Full screen",
    FullscreenCheck   => "Occuper tout l'écran (F11)" / "Fill the whole screen (F11)",
    FullscreenHint    => "Non mémorisé : l'application démarre toujours en fenêtré."
                       / "Not remembered: the application always starts windowed.",
    FrameCounter      => "Compteur d'images" / "Frame counter",
    ShowFpsCheck      => "Afficher les FPS (F)" / "Show the FPS (F)",

    // --- Audio ------------------------------------------------------------
    Mute              => "Muet" / "Mute",
    MuteCheck         => "Couper le son (M)" / "Turn the sound off (M)",
    Volume            => "Volume" / "Volume",
    VolumeHint        => "Le son est coupé pendant l'accéléré ; le volume choisi ici revient à sa libération."
                       / "Sound is off while fast-forwarding; the volume set here comes back when the key is released.",

    // --- Controls ---------------------------------------------------------
    RebindHint        => "Cliquez sur une case — ou sur le bouton dessiné — pour la réaffecter."
                       / "Click a cell — or a button on the drawing — to reassign it.",
    ResetInputs       => "Rétablir les entrées par défaut" / "Restore the default controls",
    ColumnButton      => "Bouton" / "Button",
    ColumnKeyboard    => "Clavier" / "Keyboard",
    ColumnPad         => "Manette" / "Controller",
    /// What the waiting *cell* says. Short on purpose: the whole instruction
    /// is on the accent-coloured line right above the list, and a cell wide
    /// enough for a sentence would shift its whole row out of the column.
    PressAKey         => "Touche…" / "Key…",
    PressAButton      => "Bouton…" / "Button…",
    ConflictHint      => "Une touche déjà prise par un autre bouton est échangée avec lui ; les raccourcis de l'application (F1-F12, Tab, P, M…) sont refusés."
                       / "A key another button already uses is swapped with it; the application's own shortcuts (F1-F12, Tab, P, M…) are refused.",
    PlayersHint       => "Clavier et manette 1 pilotent le joueur 1, manette 2 le joueur 2. Les sticks et la croix restent toujours actifs sur les directions."
                       / "The keyboard and controller 1 drive player 1, controller 2 drives player 2. Sticks and D-pad always work as directions.",
    PadArtHint        => "Les boutons pressés s'allument : de quoi vérifier une manette sans lancer de jeu."
                       / "Pressed buttons light up: a way to check a controller without starting a game.",
    PadArtClickHint   => "Clic : réaffecter la touche · clic droit : le bouton de manette."
                       / "Click: reassign the key · right click: the controller button.",
    PadArtShort       => "Les boutons pressés s'allument. Clic : réaffecter."
                       / "Pressed buttons light up. Click: reassign.",

    // --- The twelve SNES buttons. Only the directions are prose; the eight
    // others carry the legend printed on the pad and are never translated.
    ButtonUp          => "Haut" / "Up",
    ButtonDown        => "Bas" / "Down",
    ButtonLeft        => "Gauche" / "Left",
    ButtonRight       => "Droite" / "Right",

    // --- Emulation --------------------------------------------------------
    FastForward       => "Accéléré (Tab)" / "Fast-forward (Tab)",
    FastForwardHint   => "Nombre d'images émulées par image affichée tant que Tab est maintenu."
                       / "Frames emulated per displayed frame while Tab is held.",
    InstantResume     => "Reprise instantanée" / "Instant resume",
    InstantResumeCheck => "Reprendre où l'on s'était arrêté (F10)"
                       / "Pick up where you left off (F10)",
    InstantResumeHint => "L'état de session est écrit à chaque sortie, dans un fichier séparé des slots."
                       / "The session state is written on every exit, in a file of its own, apart from the slots.",
    Confirmation      => "Confirmation" / "Confirmation",
    ConfirmQuitCheck  => "Demander avant de quitter (C)" / "Ask before quitting (C)",
    SaveSlot          => "Slot de sauvegarde" / "Save slot",
    SaveSlotHint      => "F5 sauvegarde et F9 recharge ce slot ; F7 passe au suivant."
                       / "F5 saves and F9 reloads this slot; F7 steps to the next one.",

    // --- Folders ----------------------------------------------------------
    RomFolder         => "Dossier des ROMs" / "ROM folder",
    RomFolderHint     => "Dossier analysé par la bibliothèque de l'accueil."
                       / "The folder the home screen's library scans.",
    ScreenshotFolder  => "Dossier des captures" / "Screenshot folder",
    ScreenshotFolderHint => "Destination de F12 ; la galerie de la fiche de jeu lit le même dossier."
                       / "Where F12 writes; the game sheet's gallery reads the same folder.",
    SaveFolder        => "Dossier des sauvegardes" / "Save folder",
    SaveFolderHint    => "Sauvegardes de cartouche (.srm), slots et reprise. Pris en compte au chargement d'un jeu. Dans un dossier commun, chaque fichier porte le nom du jeu (titre de la cartouche et somme de contrôle), jamais celui du fichier ROM : deux ROMs homonymes gardent des sauvegardes distinctes. Une sauvegarde restée à côté de la ROM est toujours relue tant que le dossier n'en a pas : rien n'est déplacé ni supprimé."
                       / "Cartridge saves (.srm), slots and instant resume. Taken into account when a game is loaded. In a shared folder every file is named after the game (cartridge title and checksum), never after the ROM file: two ROMs of the same name keep separate saves. A save left beside the ROM is still read as long as the folder has none: nothing is moved or deleted.",
    Choose            => "Choisir…" / "Choose…",
    DefaultChoice     => "Par défaut" / "Default",
    BesideRom         => "À côté de la ROM" / "Beside the ROM",
    BesideRomShots    => "À côté de la ROM, dans Screenshots/" / "Beside the ROM, in Screenshots/",

    // --- About ------------------------------------------------------------
    AboutBlurb        => "Émulateur Super Nintendo écrit en Rust : cœur d'émulation sans entrées/sorties, interface séparée."
                       / "A Super Nintendo emulator written in Rust: an I/O-free emulation core, with the interface kept apart.",
    Guide             => "Guide pédagogique" / "Learning guide",
    OpenPdf           => "Ouvrir le PDF" / "Open the PDF",
    GuideMissing      => "Introuvable près de cette version : le PDF se trouve dans le dépôt, sous docs/emulateur-snes-explique.pdf."
                       / "Not found next to this build: the PDF lives in the repository, under docs/emulateur-snes-explique.pdf.",
    AppFiles          => "Fichiers de l'application" / "Application files",
    AppFilesHint      => "Préférences, cache de la bibliothèque et miniatures ; supprimables sans rien perdre."
                       / "Preferences, library cache and thumbnails; safe to delete, nothing is lost.",
    NoConfigDir       => "Aucun répertoire de configuration disponible : rien n'est mémorisé."
                       / "No configuration directory available: nothing is remembered.",

    // --- Failures the shell reports in place -------------------------------
    UnknownPadButton  => "Ce bouton n'est pas reconnu par le système."
                       / "The system does not recognise this button.",
    GuideNotFound     => "Le guide n'a pas été trouvé." / "The guide was not found.",
    LibraryThreadFailed => "La bibliothèque n'a pas pu démarrer (thread indisponible)."
                       / "The library could not start (no thread available).",

    // --- What an application shortcut does, named in the refusal notice.
    // Lower case: these are read inside a sentence, never as a label.
    HotkeyFastForward => "accéléré" / "fast-forward",
    HotkeyMute        => "muet" / "mute",
    HotkeyPause       => "pause" / "pause",
    HotkeyNextFrame   => "image suivante" / "next frame",
    HotkeyOpenRom     => "ouvrir une ROM" / "open a ROM",
    HotkeyConfirmQuit => "confirmation avant de quitter" / "confirm before quitting",
    HotkeyFrameCounter => "compteur d'images" / "frame counter",
    HotkeyFilter      => "filtre" / "filter",
    HotkeyAspect      => "ratio" / "aspect",
    HotkeySettings    => "réglages" / "settings",
    HotkeyVolumeUp    => "volume +" / "volume +",
    HotkeyVolumeDown  => "volume -" / "volume -",
    HotkeyFastForwardFactor => "facteur d'accéléré" / "fast-forward factor",
    HotkeyWindowSize  => "taille de la fenêtre" / "window size",
    HotkeySaveState   => "sauvegarder l'état" / "save state",
    HotkeyReset       => "réinitialiser la console" / "reset the console",
    HotkeyNextSlot    => "slot suivant" / "next slot",
    HotkeyExportSpc   => "exporter la musique" / "export the music",
    HotkeyLoadState   => "charger l'état" / "load state",
    HotkeyInstantResume => "reprise instantanée" / "instant resume",
    HotkeyFullscreen  => "plein écran" / "full screen",
    HotkeyScreenshot  => "capture d'écran" / "screenshot",
    HotkeySaveSlot    => "slot de sauvegarde" / "save slot",

    // --- Controller buttons, as the system reports them --------------------
    PadSouth          => "Bouton bas" / "Bottom button",
    PadEast           => "Bouton droite" / "Right button",
    PadNorth          => "Bouton haut" / "Top button",
    PadWest           => "Bouton gauche" / "Left button",
    PadLeftTrigger    => "Gâchette L (LB)" / "L shoulder (LB)",
    PadLeftTrigger2   => "Gâchette L2 (LT)" / "L trigger (LT)",
    PadRightTrigger   => "Gâchette R (RB)" / "R shoulder (RB)",
    PadRightTrigger2  => "Gâchette R2 (RT)" / "R trigger (RT)",
    PadMode           => "Mode" / "Mode",
    PadLeftThumb      => "Clic stick gauche" / "Left stick click",
    PadRightThumb     => "Clic stick droit" / "Right stick click",
    PadDPadUp         => "Croix haut" / "D-pad up",
    PadDPadDown       => "Croix bas" / "D-pad down",
    PadDPadLeft       => "Croix gauche" / "D-pad left",
    PadDPadRight      => "Croix droite" / "D-pad right",
    PadUnknown        => "Inconnu" / "Unknown",

    // --- In-game status overlay. Upper case and unaccented in both
    // languages: the overlay font (`video::glyph`) has nothing else.
    StatusResumed     => "REPRISE" / "RESUMED",
    StatusScreenshot  => "CAPTURE ECRAN" / "SCREENSHOT",
    StatusNoScreenshot => "CAPTURE IMPOSSIBLE" / "SCREENSHOT FAILED",
    StatusSpcFailed   => "EXPORT SPC ERREUR" / "SPC EXPORT ERROR",
    StatusSpcExported => "MUSIQUE SPC EXPORTEE" / "SPC MUSIC EXPORTED",

    // --- Native menu bar (macOS) -------------------------------------------
    MenuSettings      => "Réglages…" / "Settings…",
    MenuFile          => "Fichier" / "File",
    MenuHome          => "Accueil (Échap)" / "Home (Esc)",
    MenuScreenshot    => "Capture d'écran (F12)" / "Screenshot (F12)",
    MenuExportSpc     => "Exporter la musique (.spc)…" / "Export the music (.spc)…",
    MenuPauseResume   => "Pause / Reprise" / "Pause / Resume",
    MenuReset         => "Réinitialiser" / "Reset",
    MenuSaveState     => "Sauvegarder l'état (F5)" / "Save state (F5)",
    MenuLoadState     => "Charger l'état (F9)" / "Load state (F9)",
    MenuNextSlot      => "Slot suivant (F7)" / "Next slot (F7)",
    MenuFullscreen    => "Plein écran (F11)" / "Full screen (F11)",
}

/// What the footer says Escape does. Written out per language and per state
/// rather than assembled: with no cartridge loaded Escape leaves the
/// application, and the line must not promise otherwise.
pub fn escape_hint(lang: Lang, has_session: bool) -> &'static str {
    match (lang, has_session) {
        (Lang::Fr, true) => "Échap : revenir au jeu · O : ouvrir une ROM · , : réglages",
        (Lang::Fr, false) => "O : ouvrir une ROM · , : réglages · Échap : quitter",
        (Lang::En, true) => "Esc: back to the game · O: open a ROM · ,: settings",
        (Lang::En, false) => "O: open a ROM · ,: settings · Esc: quit",
    }
}

/// The suspended session, on the header chip.
pub fn resume_chip(lang: Lang, title: &str) -> String {
    match lang {
        Lang::Fr => format!("Reprendre · {title}"),
        Lang::En => format!("Resume · {title}"),
    }
}

/// The menu entry that leaves the application, named after it.
pub fn quit_named(lang: Lang, app_name: &str) -> String {
    match lang {
        Lang::Fr => format!("Quitter {app_name}"),
        Lang::En => format!("Quit {app_name}"),
    }
}

/// Title of the quit confirmation.
pub fn quit_app(lang: Lang, app_name: &str) -> String {
    match lang {
        Lang::Fr => format!("Quitter {app_name} ?"),
        Lang::En => format!("Quit {app_name}?"),
    }
}

/// Thumbnails still being emulated. Written out per count rather than with a
/// parenthesised plural — this is prose.
pub fn thumbnails_pending(lang: Lang, count: usize) -> String {
    match (lang, count) {
        (Lang::Fr, 1) => "1 miniature en cours…".to_string(),
        (Lang::Fr, n) => format!("{n} miniatures en cours…"),
        (Lang::En, 1) => "1 thumbnail being built…".to_string(),
        (Lang::En, n) => format!("{n} thumbnails being built…"),
    }
}

/// The expert entry of the window-size ladder, named for what it is. `dims` is
/// the picture it produces and stays a machine value.
pub fn native_size(lang: Lang, dims: &str) -> String {
    match lang {
        Lang::Fr => format!("Taille native ({dims})"),
        Lang::En => format!("Native size ({dims})"),
    }
}

/// A slot of the save-state list, or the automatic session state.
pub fn slot_label(lang: Lang, slot: Option<u8>) -> String {
    match (lang, slot) {
        (_, Some(n)) => format!("Slot {n}"),
        (lang, None) => Msg::ResumeState.text(lang).to_string(),
    }
}

/// The save folder the player just left, whose files are still read.
pub fn previous_save_dir(lang: Lang, path: &str) -> String {
    match lang {
        Lang::Fr => format!("Dossier précédent, toujours relu : {path}"),
        Lang::En => format!("Previous folder, still read: {path}"),
    }
}

/// A folder that could not be used, so the setting was left alone.
pub fn unusable_folder(lang: Lang, error: &str) -> String {
    match lang {
        Lang::Fr => format!("Dossier inutilisable, réglage inchangé : {error}"),
        Lang::En => format!("Unusable folder, the setting is unchanged: {error}"),
    }
}

/// A file the platform refused to open.
pub fn cannot_open(lang: Lang, error: &str) -> String {
    match lang {
        Lang::Fr => format!("Ouverture impossible : {error}"),
        Lang::En => format!("Could not open it: {error}"),
    }
}

/// Waiting for a key or a pad button to bind to `button`.
///
/// Written out per language rather than assembled: French puts the button after
/// When the session being offered was left. `when` is already localised.
pub fn resume_from(lang: Lang, when: &str) -> String {
    match lang {
        Lang::Fr => format!("Reprendre la partie laissée le {when}"),
        Lang::En => format!("Pick up the session left on {when}"),
    }
}

/// Waiting for a key or a pad button to bind to `button`. Written out per
/// language rather than assembled: French puts the button after the verb
/// phrase, English fronts it, and a shared template would force one of them
/// into the other's word order.
pub fn press_a_key_for(lang: Lang, button: &str) -> String {
    match lang {
        Lang::Fr => format!("Appuyez sur une touche pour {button} — Échap pour annuler."),
        Lang::En => format!("Press a key for {button} — Esc to cancel."),
    }
}

/// The same, waiting on a controller instead.
pub fn press_a_pad_button_for(lang: Lang, button: &str) -> String {
    match lang {
        Lang::Fr => {
            format!("Appuyez sur un bouton de manette pour {button} — Échap pour annuler.")
        }
        Lang::En => format!("Press a controller button for {button} — Esc to cancel."),
    }
}

/// A key refused because the application already acts on it.
pub fn key_is_reserved(lang: Lang, key: &str, what: &str) -> String {
    match lang {
        Lang::Fr => format!("{key} est déjà un raccourci de l'application ({what})."),
        Lang::En => format!("{key} is already an application shortcut ({what})."),
    }
}

/// Two buttons traded their bindings, which the player did not ask for
/// directly.
pub fn binding_swapped(lang: Lang, binding: &str, other: &str) -> String {
    match lang {
        Lang::Fr => {
            format!("{binding} servait déjà pour {other} : les deux boutons ont été échangés.")
        }
        Lang::En => format!("{binding} already drove {other}: the two buttons were swapped."),
    }
}

/// The other button had nothing to receive in exchange and went back to its
/// built-in binding.
pub fn binding_reverted(lang: Lang, binding: &str, other: &str) -> String {
    match lang {
        Lang::Fr => format!(
            "{binding} servait déjà pour {other} : ce bouton revient à son réglage par défaut."
        ),
        Lang::En => {
            format!("{binding} already drove {other}: that button goes back to its default.")
        }
    }
}

/// When the new save folder takes effect, and what became of the files in the
/// one being replaced.
pub fn save_dir_notice(lang: Lang, game_running: bool, previous: Option<&str>) -> String {
    let mut text = match (lang, game_running) {
        (Lang::Fr, true) => {
            "Pris en compte au prochain chargement de jeu : la partie en cours garde ses fichiers actuels.".to_string()
        }
        (Lang::Fr, false) => "Pris en compte au prochain chargement de jeu.".to_string(),
        (Lang::En, true) => {
            "Taken into account at the next game load: the running game keeps its current files."
                .to_string()
        }
        (Lang::En, false) => "Taken into account at the next game load.".to_string(),
    };
    if let Some(previous) = previous {
        text.push_str(&match lang {
            Lang::Fr => format!(
                " Les sauvegardes restées dans {previous} sont toujours relues ; rien n'a été déplacé ni supprimé."
            ),
            Lang::En => format!(
                " The saves left in {previous} are still read; nothing was moved or deleted."
            ),
        });
    }
    text
}

/// The in-game overlay's save-slot lines. Upper case and unaccented, like every
/// other string that font draws.
pub fn status_slot(lang: Lang, slot: u8, what: SlotStatus) -> String {
    let word = match (lang, what) {
        (Lang::Fr, SlotStatus::Saved) => "SAUVE",
        (Lang::Fr, SlotStatus::Loaded) => "CHARGE",
        (Lang::Fr, SlotStatus::Empty) => "VIDE",
        (Lang::Fr, SlotStatus::Failed) => "ERREUR",
        (Lang::Fr, SlotStatus::Selected) => "",
        (Lang::En, SlotStatus::Saved) => "SAVED",
        (Lang::En, SlotStatus::Loaded) => "LOADED",
        (Lang::En, SlotStatus::Empty) => "EMPTY",
        (Lang::En, SlotStatus::Failed) => "ERROR",
        (Lang::En, SlotStatus::Selected) => "",
    };
    if word.is_empty() {
        format!("SLOT {slot}")
    } else {
        format!("SLOT {slot} {word}")
    }
}

/// What happened to a save slot, for `status_slot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotStatus {
    Saved,
    Loaded,
    Empty,
    Failed,
    /// Only the slot changed; nothing was written or read.
    Selected,
}

/// A controller plugged in or unplugged, on the in-game overlay.
pub fn status_pad(lang: Lang, player: usize, connected: bool) -> String {
    match (lang, connected) {
        (Lang::Fr, true) => format!("MANETTE {player} CONNECTEE"),
        (Lang::Fr, false) => format!("MANETTE {player} DECONNECTEE"),
        (Lang::En, true) => format!("CONTROLLER {player} CONNECTED"),
        (Lang::En, false) => format!("CONTROLLER {player} DISCONNECTED"),
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

    /// The catalogue only earns its ceremony if the screens actually go
    /// through it: a French sentence left inline compiles, renders, and is
    /// noticed on a screenshot months later. So the source of every screen is
    /// read here and any accented literal outside its test module fails the
    /// build — the same shape of guard as `dialog.rs`'s
    /// `the_event_loop_never_calls_the_native_picker_itself`.
    ///
    /// Accents rather than a word list because they are what French cannot
    /// avoid and English never has: `Réglages`, `Entrées`, `Échap`, `déjà`.
    /// A French sentence with no accent at all would slip through — hence the
    /// per-screen assertions in the screens' own tests as well.
    #[test]
    fn no_screen_of_the_shell_holds_a_french_string_of_its_own() {
        const SOURCES: [(&str, &str); 13] = [
            ("ui/home.rs", include_str!("ui/home.rs")),
            ("ui/library_view.rs", include_str!("ui/library_view.rs")),
            ("ui/game_sheet.rs", include_str!("ui/game_sheet.rs")),
            ("ui/settings.rs", include_str!("ui/settings.rs")),
            ("ui/confirm.rs", include_str!("ui/confirm.rs")),
            ("ui/tabs.rs", include_str!("ui/tabs.rs")),
            ("ui/pad_art.rs", include_str!("ui/pad_art.rs")),
            ("ui/game.rs", include_str!("ui/game.rs")),
            ("ui/shot.rs", include_str!("ui/shot.rs")),
            ("input.rs", include_str!("input.rs")),
            ("pad.rs", include_str!("pad.rs")),
            ("library.rs", include_str!("library.rs")),
            ("menu.rs", include_str!("menu.rs")),
        ];
        let mut read = 0;
        for (name, source) in SOURCES {
            let literals = string_literals(before_tests(source));
            read += literals.len();
            for literal in literals {
                assert!(
                    !literal.chars().any(is_accented),
                    "{name} carries the French string {literal:?} instead of a Msg"
                );
            }
        }
        // A scanner that read nothing would pass every file in silence, so it
        // has to be caught reading: this literal is a machine value, which is
        // why it is still inline.
        assert!(read > 100, "only {read} literals read across the shell");
        let settings = string_literals(before_tests(include_str!("ui/settings.rs")));
        assert!(settings.iter().any(|l| l == "256 × 224"), "the scanner reads nothing useful");
    }

    /// Everything up to the test module: an assertion written in French is a
    /// test asserting French, not a screen speaking it.
    fn before_tests(source: &str) -> &str {
        match source.find("#[cfg(test)]") {
            Some(cut) => &source[..cut],
            None => source,
        }
    }

    fn is_accented(c: char) -> bool {
        "éèêëàâäçîïôöùûüœÉÈÊËÀÂÄÇÎÏÔÖÙÛÜŒ".contains(c)
    }

    /// Every double-quoted literal of `source`, comments and character
    /// literals left out.
    fn string_literals(source: &str) -> Vec<String> {
        let chars: Vec<char> = source.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '/' if chars.get(i + 1) == Some(&'/') => {
                    while i < chars.len() && chars[i] != '\n' {
                        i += 1;
                    }
                }
                // A character literal, or a lifetime — both are stepped over
                // the same way, since neither can hold a double quote we care
                // about.
                '\'' => {
                    i += 1;
                    if chars.get(i) == Some(&'\\') {
                        i += 1;
                    }
                    i += 1;
                    if chars.get(i) == Some(&'\'') {
                        i += 1;
                    }
                }
                '"' => {
                    let mut literal = String::new();
                    i += 1;
                    while i < chars.len() && chars[i] != '"' {
                        if chars[i] == '\\' {
                            i += 1;
                        }
                        if i < chars.len() {
                            literal.push(chars[i]);
                            i += 1;
                        }
                    }
                    i += 1;
                    out.push(literal);
                }
                _ => i += 1,
            }
        }
        out
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
