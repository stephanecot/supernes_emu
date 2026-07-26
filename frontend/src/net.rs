//! The application's only outbound network access.
//!
//! Everything else in this frontend reads the disk. This module exists so that
//! "does Prisme talk to the internet?" has one answer with one file to read:
//! nothing here runs unless `metadata` is asked to fill a sheet in, and
//! `metadata` is only asked by a button the player pressed
//! (`ui::Action::FillSheet` / `FillLibrary`). Nothing at scan time, nothing at
//! startup.
//!
//! **Why `ureq` with rustls.** Blocking (the caller is already a background
//! thread, so there is nothing for an async runtime to overlap with), pure Rust
//! (no OpenSSL to find on three platforms), and small: sixteen crates, of which
//! `ring` is the only one carrying C. Checked before committing to it — 3.3.0,
//! published 2026-03, and both hosts this project reads
//! (`raw.githubusercontent.com`, `en.wikipedia.org`) answered over TLS from a
//! test binary built here.
//!
//! **Every call is bounded**: a connect timeout, a global timeout covering
//! redirects and the body, and a byte cap. A hung server therefore ends one
//! background job, never the session — and the caller runs on
//! `library::Worker`, so no failure mode of this module can reach the UI
//! thread at all.

use std::time::Duration;

/// Sent on every request. A tool that fetches from public servers says what it
/// is, so an operator seeing the traffic can tell it from a scraper.
pub const USER_AGENT: &str =
    concat!("Prisme/", env!("CARGO_PKG_VERSION"), " (Super Nintendo emulator; library metadata)");

/// Longest a connection may take to establish.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest one whole request may take, redirects and body included.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(45);

/// Byte cap on a response body. The largest thing fetched is a No-Intro DAT
/// (about one megabyte); box art runs to a few hundred kilobytes. Ten is
/// generous for both and still refuses a server that answers with a stream.
pub const MAX_BODY: u64 = 10 * 1024 * 1024;

/// Why a fetch produced nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The server answered, and the answer is "there is no such thing here".
    /// A legitimate outcome, not a failure: most games have no ESRB rating and
    /// plenty have no Wikipedia article.
    NotFound,
    /// Anything else: no route, TLS refused, a 500, a timeout, a body past the
    /// cap. Carries a message for the log, never for a modal.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => f.write_str("not found"),
            Error::Failed(e) => f.write_str(e),
        }
    }
}

/// What `metadata` needs from the network, as a trait so the whole fetch chain
/// can be exercised offline against a table of canned responses (see
/// `metadata`'s tests). The real implementation is `Http`.
pub trait Fetch {
    fn get(&self, url: &str) -> Result<Vec<u8>, Error>;
}

/// A live HTTP client. One agent, so its connection pool is reused across the
/// dozens of requests a "fill the whole library" pass makes.
pub struct Http {
    agent: ureq::Agent,
}

impl Http {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .user_agent(USER_AGENT)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(TOTAL_TIMEOUT))
            // A 404 is data here, not an exception: `get` maps it to
            // `NotFound` itself rather than letting ureq raise it.
            .http_status_as_error(false)
            .build();
        Self { agent: config.into() }
    }
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetch for Http {
    fn get(&self, url: &str) -> Result<Vec<u8>, Error> {
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| Error::Failed(format!("{url}: {e}")))?;
        let status = response.status().as_u16();
        if status == 404 || status == 410 {
            return Err(Error::NotFound);
        }
        if !(200..300).contains(&status) {
            return Err(Error::Failed(format!("{url}: HTTP {status}")));
        }
        response
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_vec()
            .map_err(|e| Error::Failed(format!("{url}: {e}")))
    }
}

/// Percent-encode one path segment: everything outside the unreserved set of
/// RFC 3986 is escaped, `/` included, since these segments are file names that
/// may hold spaces, apostrophes, commas and parentheses (`Super Mario World 2 -
/// Yoshi's Island (Europe) (En,Fr,De) (Rev 1).png`).
pub fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_segment_escapes_everything_a_game_name_can_hold() {
        assert_eq!(encode_segment("Super Mario Kart (Europe).png"), "Super%20Mario%20Kart%20%28Europe%29.png");
        assert_eq!(encode_segment("Yoshi's Island (En,Fr,De)"), "Yoshi%27s%20Island%20%28En%2CFr%2CDe%29");
        // Unreserved characters are left exactly as they are…
        assert_eq!(encode_segment("aZ0-_.~"), "aZ0-_.~");
        // …and a separator inside a name is escaped rather than opening a path.
        assert_eq!(encode_segment("a/b"), "a%2Fb");
        // Non-ASCII goes out as its UTF-8 bytes.
        assert_eq!(encode_segment("é"), "%C3%A9");
        assert_eq!(encode_segment(""), "");
    }

    #[test]
    fn the_user_agent_names_the_application_and_its_version() {
        assert!(USER_AGENT.starts_with("Prisme/"), "{USER_AGENT}");
        assert!(USER_AGENT.contains(env!("CARGO_PKG_VERSION")), "{USER_AGENT}");
    }

    #[test]
    fn a_missing_page_is_told_apart_from_a_broken_one() {
        assert_eq!(Error::NotFound.to_string(), "not found");
        assert_ne!(Error::NotFound, Error::Failed("not found".to_string()));
        assert_eq!(Error::Failed("boom".to_string()).to_string(), "boom");
    }

    /// Both hosts, over TLS, from wherever this runs. Ignored by default: it is
    /// the one test in the tree that needs the network. Run with
    /// `cargo test -p prisme -- --ignored reaches_the_real`.
    #[test]
    #[ignore]
    fn reaches_the_real_servers_over_tls() {
        let http = Http::new();
        let dat = http
            .get("https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/franchise/Nintendo%20-%20Super%20Nintendo%20Entertainment%20System.dat")
            .expect("libretro-database");
        assert!(dat.starts_with(b"clrmamepro"), "{:?}", &dat[..32.min(dat.len())]);
        let summary = http
            .get("https://en.wikipedia.org/api/rest_v1/page/summary/Terranigma")
            .expect("wikipedia");
        assert!(String::from_utf8_lossy(&summary).contains("\"extract\""));
        // A page that does not exist must come back as `NotFound`, which is
        // what stops the caller from treating it as a broken connection.
        assert_eq!(
            http.get("https://en.wikipedia.org/api/rest_v1/page/summary/Zzz_no_such_page_98765")
                .unwrap_err(),
            Error::NotFound
        );
    }
}
