//! The assistant: asking, in plain words, for something to be found or done.
//!
//! `agent.rs` gave an outside program hands and eyes — step, press, look at the
//! screen, read and write memory, save and restore. This module supplies the
//! head, and the place to talk to it.
//!
//! **What runs.** The `claude` command-line tool, if the player has it, in its
//! non-interactive mode, as a child process on their own machine. No API key
//! belongs in this emulator and none is asked for: the session already
//! installed does the reasoning. The feature therefore exists only when that
//! tool does — detected once, and said plainly rather than failing on click.
//!
//! **What it is given.** A save state, the path of the emulator binary, and the
//! sentence the player typed. Not the ROM, not the library, not the
//! preferences: an assistant that needs a cartridge dump to count lives has
//! been given too much.
//!
//! **What it gives back.** For a cheat, a `<game>.cheats.json` the shell
//! already knows how to read — nothing of the running session is touched, so
//! the worst outcome is that nothing was found. Playing a passage is the
//! riskier shape and is handled by the caller, which keeps a state from before.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// File names the tool can have. Windows does not mark a file executable, it
/// names it: an npm-installed CLI lands there as a `.cmd` shim next to the
/// `.exe`, and looking only for a bare `claude` finds neither.
#[cfg(windows)]
const CLAUDE_NAMES: &[&str] = &["claude.exe", "claude.cmd", "claude.bat", "claude"];
#[cfg(not(windows))]
const CLAUDE_NAMES: &[&str] = &["claude"];

// There is deliberately **no** list of likely install directories. Guessing
// where a tool lives is magic that works until it does not, and it hides the
// one thing worth knowing when it fails: where the application actually
// looked. The tool is on the `PATH` or it is named by hand — nothing in
// between.
//
// The consequence is real and must be met head-on rather than papered over: a
// windowed application does not inherit a shell's `PATH`. On macOS the Finder
// hands it a bare login environment, so a tool installed in `~/.local/bin` is
// invisible to the bundle while working perfectly in a terminal. That is
// exactly the case `Prefs::assistant_path` exists for.

/// What the player is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// Find a memory address and leave a cheat behind. Touches nothing of the
    /// running session.
    Cheat,
    /// Play from the saved state and hand back where it got to.
    Play,
}

/// Where the assistant is, from the shell's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// A line of narration, in the assistant's own words.
    Progress(String),
    /// Finished, with what it has to say about it.
    Done(String),
    /// Failed, or was stopped. The message is shown as-is.
    Failed(String),
}

/// Where the tool usually lands on this platform, as a starting value for the
/// settings field. Shown and editable rather than searched behind the scenes:
/// the player can see what is being looked at, and change it in one gesture.
/// Empty when the home directory is unknown, which reads as "look on the
/// `PATH`" like any other empty field.
pub fn default_path() -> String {
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else {
        return String::new();
    };
    let home = PathBuf::from(home);
    #[cfg(windows)]
    let candidate = home.join("AppData").join("Roaming").join("npm").join("claude.cmd");
    #[cfg(not(windows))]
    let candidate = home.join(".local").join("bin").join("claude");
    candidate.display().to_string()
}

/// Locate the tool. `None` disables the feature — which the settings screen
/// says out loud, since a greyed-out button with no reason is worse than an
/// absent one.
pub fn find_claude(chosen: Option<&Path>) -> Option<PathBuf> {
    // A path the player named themselves wins, and is not second-guessed: they
    // know where they installed it better than any search does.
    if let Some(path) = chosen {
        return is_executable(path).then(|| path.to_path_buf());
    }
    CLAUDE_NAMES.iter().find_map(|name| which(name))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Everything one request needs. Built by the shell, which owns the paths.
#[derive(Debug, Clone)]
pub struct Request {
    pub task: Task,
    /// What the player typed, verbatim.
    pub wish: String,
    /// The emulator binary the assistant drives — resolved from
    /// `current_exe`, so an assistant launched from a bundle drives that same
    /// bundle rather than whatever a `PATH` lookup would have found.
    pub emulator: PathBuf,
    pub rom: PathBuf,
    /// State the assistant starts from: the session as it is right now.
    pub state: PathBuf,
    /// Where a found cheat must be written.
    pub cheats: PathBuf,
}

/// The instructions handed to the assistant.
///
/// Deliberately concrete: the exact command line, the exact JSON, and the two
/// failure modes that waste the most time — a control round run while the game
/// is still resolving the event, and a candidate that is a copy rather than the
/// value the game reads. Both cost a real search on this project before they
/// were written down (`docs/CHEATS.md`).
pub fn prompt(request: &Request) -> String {
    let Request { task, wish, emulator, rom, state, cheats } = request;
    let (emulator, rom, state, cheats) = (
        emulator.display(),
        rom.display(),
        state.display(),
        cheats.display(),
    );
    let common = format!(
        "You are driving a SNES emulator on behalf of someone playing a game.\n\
         They asked, in their own words: \"{wish}\"\n\n\
         Start the control channel with:\n\
         \x20 {emulator} \"{rom}\" --agent --load-state \"{state}\"\n\
         It speaks one JSON object per line on stdin and answers one per line on\n\
         stdout. Send {{\"cmd\":\"help\"}} first: it lists every command and its\n\
         exact shape. The emulation is deterministic, so replaying the same\n\
         inputs from the same state gives the same result — that is what makes\n\
         any search you do reproducible.\n\n\
         Report what you did in one short paragraph, in the language the wish\n\
         above is written in. If you did not succeed, say so plainly and say\n\
         what you ruled out. Do not claim a result you did not verify.\n"
    );
    match task {
        Task::Cheat => format!(
            "{common}\n\
             Your job: find the memory address behind that wish, and leave a\n\
             cheat behind.\n\n\
             Read docs/CHEATS.md — it describes the intersection search step by\n\
             step, and carries two warnings that each cost a wasted search:\n\
             a control round run while the game is still resolving the event\n\
             eliminates the right address, and several addresses survive every\n\
             round because they are copies — only writing to them tells you\n\
             which one the game actually reads.\n\n\
             When you are sure, persist it with cheat-add, whose file is:\n\
             \x20 {cheats}\n\
             Then verify in a *fresh* process that only reads that file.\n"
        ),
        Task::Play => format!(
            "{common}\n\
             Your job: play from that state and get past what they asked about.\n\n\
             Look at the screen with screenshot and read the PNG before deciding\n\
             what to press — you cannot play what you have not looked at. Take a\n\
             save state whenever you make progress, so a mistake costs one\n\
             attempt and not the whole run.\n\n\
             When you are done, leave the final state at:\n\
             \x20 {state}.assistant\n\
             That file is what the player comes back to, so only write it once\n\
             you are satisfied with where the game is.\n"
        ),
    }
}

/// A request in flight. Dropping it kills the child: an assistant nobody is
/// waiting for any more must not keep emulating in the background.
pub struct Session {
    child: Option<Child>,
    updates: Receiver<Status>,
    cancelled: Arc<AtomicBool>,
}

impl Session {
    /// Start the tool on `request`. The child inherits no stdin of its own —
    /// the prompt is an argument, not a conversation — so it can never block
    /// waiting for input nobody will type.
    pub fn start(claude: &Path, request: &Request) -> Result<Self, String> {
        let mut child = Command::new(claude)
            .arg("-p")
            .arg(prompt(request))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: {e}", claude.display()))?;

        let stdout = child.stdout.take().ok_or("no stdout on the assistant")?;
        let (tx, updates) = channel();
        let cancelled = Arc::new(AtomicBool::new(false));

        let flag = Arc::clone(&cancelled);
        std::thread::Builder::new()
            .name("prisme-assistant".to_string())
            .spawn(move || pump(stdout, &tx, &flag))
            .map_err(|e| format!("could not start the assistant thread: {e}"))?;

        Ok(Self { child: Some(child), updates, cancelled })
    }

    /// Everything said since the last call. Never blocks.
    pub fn poll(&mut self) -> Vec<Status> {
        self.updates.try_iter().collect()
    }

    /// Stop now. The player is in charge: an assistant that cannot be
    /// interrupted is one you have to wait out, and waiting out a thing that
    /// looks at every frame is intolerable.
    pub fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
    }

    /// True once the child has exited, whatever its outcome.
    pub fn finished(&mut self) -> bool {
        match &mut self.child {
            Some(child) => matches!(child.try_wait(), Ok(Some(_)) | Err(_)),
            None => true,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel();
        if let Some(child) = &mut self.child {
            let _ = child.wait();
        }
    }
}

/// Forward the child's narration line by line. Its last non-empty line is the
/// summary it was asked for, so it is kept apart from the rest.
fn pump(stdout: std::process::ChildStdout, tx: &Sender<Status>, cancelled: &AtomicBool) {
    let mut last = String::new();
    for line in BufReader::new(stdout).lines() {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        last = trimmed.to_string();
        if tx.send(Status::Progress(last.clone())).is_err() {
            return;
        }
    }
    if cancelled.load(Ordering::Relaxed) {
        return;
    }
    let _ = tx.send(if last.is_empty() {
        Status::Failed("the assistant said nothing".to_string())
    } else {
        Status::Done(last)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(task: Task) -> Request {
        Request {
            task,
            wish: "des vies infinies".to_string(),
            emulator: PathBuf::from("/Applications/Prisme.app/Contents/MacOS/prisme"),
            rom: PathBuf::from("/Users/vous/Jeux/Super Mario World.zip"),
            state: PathBuf::from("/tmp/session.state"),
            cheats: PathBuf::from("/tmp/game.cheats.json"),
        }
    }

    #[test]
    fn the_prompt_carries_the_wish_verbatim_and_the_exact_command() {
        let p = prompt(&request(Task::Cheat));
        // The player's own words, unrewritten: a paraphrase is a translation
        // nobody asked for, and "infinite lives" is not always what they meant.
        assert!(p.contains("des vies infinies"), "{p}");
        assert!(p.contains("--agent --load-state"), "{p}");
        assert!(p.contains("/tmp/session.state"), "{p}");
        assert!(p.contains("/tmp/game.cheats.json"), "{p}");
    }

    #[test]
    fn each_task_is_told_what_to_leave_behind() {
        let cheat = prompt(&request(Task::Cheat));
        assert!(cheat.contains("cheat-add"), "a cheat run must persist its find");
        assert!(cheat.contains("docs/CHEATS.md"), "the two known traps are written down there");

        let play = prompt(&request(Task::Play));
        assert!(play.contains(".assistant"), "a play run must leave a state");
        assert!(!play.contains("cheat-add"), "playing is not searching");
    }

    /// Both prompts end on the same instruction, and it is the one that keeps
    /// the report honest.
    #[test]
    fn the_assistant_is_told_not_to_claim_what_it_did_not_verify() {
        for task in [Task::Cheat, Task::Play] {
            let p = prompt(&request(task));
            assert!(p.contains("Do not claim a result you did not verify"), "{task:?}");
            assert!(p.contains("say so plainly"), "{task:?}");
        }
    }

    #[test]
    fn an_absent_tool_disables_the_feature_instead_of_guessing_a_path() {
        // `find_claude` returns a path that exists and runs, or nothing at all;
        // it never hands back a hopeful name for `Command` to fail on later.
        if let Some(path) = find_claude(None) {
            assert!(is_executable(&path), "{}", path.display());
        }
    }

    #[test]
    fn a_directory_is_not_an_executable() {
        assert!(!is_executable(&std::env::temp_dir()));
        assert!(!is_executable(&std::env::temp_dir().join("definitely-not-here")));
    }

    /// The lookup must not assume a Unix layout: Windows names an executable
    /// rather than marking it, and an npm-installed CLI lands there as a
    /// `.cmd` shim. Getting this wrong gives a feature that works from a
    /// terminal and is absent from the application, which is the least
    /// debuggable failure of the lot.
    #[test]
    fn the_lookup_knows_the_shapes_this_platform_uses() {
        #[cfg(windows)]
        {
            assert!(CLAUDE_NAMES.contains(&"claude.exe"));
            assert!(CLAUDE_NAMES.contains(&"claude.cmd"));
        }
        #[cfg(not(windows))]
        assert_eq!(CLAUDE_NAMES, &["claude"]);

        // And nothing is guessed: an absent tool is absent, not looked for in
        // a list of likely places.
        assert_eq!(find_claude(Some(Path::new("/definitely/not/here"))), None);
    }
}
