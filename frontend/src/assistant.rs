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
    Arc, Mutex,
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

/// Model the assistant runs on unless the player names another.
///
/// The cheapest and fastest of the family, on purpose: this work is a long
/// series of small, mechanical steps — read a screen, press a button, compare
/// two memory dumps — repeated hundreds of times. Latency is what the player
/// feels here, and a heavier model would spend more of it on every one of
/// those steps without changing what the step decides. Someone who hits a task
/// it cannot handle names a stronger one in the settings, which is a single
/// field away.
pub const DEFAULT_MODEL: &str = "haiku";

/// What the player is asking for. One job now: playing a passage was removed
/// — an assistant that must look at a screenshot before every decision is slow
/// by construction, not by configuration, and watching it inch forward was
/// worse than playing the passage oneself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// Find a memory address and leave a cheat behind. Touches nothing of the
    /// running session.
    Cheat,
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
    /// Model to run on, or `None` for the tool's own default. Not this
    /// application's business to pick one, but its business to pass on what
    /// the player picked.
    pub model: Option<String>,
}

/// The instructions handed to the assistant.
///
/// Deliberately concrete, and deliberately self-contained: the exact command
/// line, the exact JSON, and the two failure modes that waste the most time —
/// a control round run while the game is still resolving the event, and a
/// candidate that is a copy rather than the value the game reads. Both cost a
/// real search on this project.
///
/// They are spelled out here rather than behind a pointer to `docs/CHEATS.md`:
/// an installed application has no repository beside it, so that file would be
/// an instruction to read something that is not there.
pub fn prompt(request: &Request) -> String {
    let Request { task, wish, emulator, rom, state, cheats, model: _ } = request;
    let (emulator, rom, state, cheats) = (
        emulator.display(),
        rom.display(),
        state.display(),
        cheats.display(),
    );
    // A console of its own, seeded from the saved state: the search must be
    // free to die, reload and try again without any of that reaching the game
    // the player left running.
    let start = format!("{emulator} \"{rom}\" --agent --load-state \"{state}\"");
    let common = format!(
        "You are driving a SNES emulator on behalf of someone playing a game.\n\
         They asked, in their own words: \"{wish}\"\n\n\
         Start the control channel with:\n\
         \x20 {start}\n\
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
             The method is successive intersection: snapshot the whole of WRAM\n\
             ($7E:0000, 128 KB), let the event happen, snapshot again, and keep\n\
             only the addresses that moved as expected. Repeat until few remain.\n\n\
             Two warnings, each of which has already cost a whole search:\n\
             - A control round run while the game is still *resolving* the\n\
               event eliminates the right address. Let it settle first.\n\
             - Several addresses survive every round because they are copies\n\
               (a saved counter, the tile that draws the digit). No number of\n\
               rounds separates them — only writing to each one and looking at\n\
               the screen tells you which the game actually reads.\n\n\
             When you are sure, persist it with cheat-add, whose file is:\n\
             \x20 {cheats}\n\
             Then verify in a *fresh* process that only reads that file.\n"
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
        let mut command = Command::new(claude);
        // Without this the tool has no permission to act, and — measured, not
        // supposed — it then *reports success anyway*: asked to drive the
        // emulator it answered "stepped 60 frames, quit cleanly" while nothing
        // ever connected. A silent refusal would have been merely useless; a
        // confident false report is worse, and this flag is what prevents it.
        //
        // `Bash` runs the attach client, `Read` looks at the screenshots — an
        // assistant that cannot see the screen cannot play. Nothing else is
        // granted: it has no business in the player's files.
        command.arg("--allowedTools").arg("Bash").arg("Read");
        // A process spawned from a bundle inherits `/` as its working
        // directory, which is neither writable nor a sane place to work from.
        // The application's own data directory is both, and is ours.
        if let Some(dir) = crate::prefs::data_path("") {
            if std::fs::create_dir_all(&dir).is_ok() {
                command.current_dir(&dir);
            }
        }
        if let Some(model) = &request.model {
            command.arg("--model").arg(model);
        }
        let mut child = command
            .arg("-p")
            .arg(prompt(request))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: {e}", claude.display()))?;

        // stderr is *drained*, never merely piped: nothing reads a pipe that
        // nobody drains, so the 64 KB buffer fills and the child blocks there
        // for good — the assistant starts, says nothing, and hangs. Its last
        // lines are also the only explanation available when it fails.
        let stderr = child.stderr.take().ok_or("no stderr on the assistant")?;
        let complaints = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&complaints);
        std::thread::Builder::new()
            .name("prisme-assistant-err".to_string())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Ok(mut held) = sink.lock() {
                        // Only the tail is worth keeping: a failure explains
                        // itself at the end, and an unbounded string would grow
                        // with every warning a long run emits.
                        if held.len() > 4096 {
                            held.clear();
                        }
                        held.push_str(line.trim());
                        held.push('\n');
                    }
                }
            })
            .map_err(|e| format!("could not drain the assistant's errors: {e}"))?;

        let stdout = child.stdout.take().ok_or("no stdout on the assistant")?;
        let (tx, updates) = channel();
        let cancelled = Arc::new(AtomicBool::new(false));

        let flag = Arc::clone(&cancelled);
        let errors = Arc::clone(&complaints);
        std::thread::Builder::new()
            .name("prisme-assistant".to_string())
            .spawn(move || pump(stdout, &tx, &flag, &errors))
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
fn pump(
    stdout: std::process::ChildStdout,
    tx: &Sender<Status>,
    cancelled: &AtomicBool,
    complaints: &Mutex<String>,
) {
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
    let _ = tx.send(if !last.is_empty() {
        Status::Done(last)
    } else {
        // Nothing on stdout means it failed before saying anything; what it
        // wrote to stderr is then the only account of why, and reporting
        // "said nothing" while holding the reason would be a lie of omission.
        let why = complaints.lock().ok().map(|held| held.trim().to_string()).unwrap_or_default();
        Status::Failed(if why.is_empty() {
            "the assistant said nothing".to_string()
        } else {
            why.lines().last().unwrap_or(&why).to_string()
        })
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
            model: None,
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
        // Spelled out, not pointed at: an installed application has no
        // repository next to it.
        assert!(!cheat.contains("docs/CHEATS.md"), "a deployed app cannot read that file");
        assert!(cheat.contains("successive intersection"), "the method must be in the prompt");
        assert!(cheat.contains("copies"), "and the trap that costs a whole search");
    }

    /// The instruction that keeps the report honest — and it is not
    /// decoration: measured, an assistant with no permission to act reported
    /// "stepped 60 frames, quit cleanly" while nothing had ever connected.
    #[test]
    fn the_assistant_is_told_not_to_claim_what_it_did_not_verify() {
        let p = prompt(&request(Task::Cheat));
        assert!(p.contains("Do not claim a result you did not verify"), "{p}");
        assert!(p.contains("say so plainly"), "{p}");
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
