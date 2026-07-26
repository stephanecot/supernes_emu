//! The client half of the live channel, exercised as the assistant actually
//! runs it: the real binary, in `--agent-attach` mode, against a listener that
//! plays the part of the running application.
//!
//! The server half is covered by `live.rs`'s own harness, which drives a real
//! socket against a real console. What only this test can reach is the command
//! line: that the secret leaves through the environment, that the first line on
//! the wire is that secret and nothing else, and that stdin and stdout are
//! forwarded verbatim in both directions — which is the entire contract the
//! prompt makes with `claude`.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SECRET: &str = "0123456789abcdef0123456789abcdef";

/// Kill the child whatever the assertions do: a test that fails must not leave
/// a process attached to a socket nobody is listening on any more.
struct Attached(Child);

impl Drop for Attached {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_attach_client_presents_the_secret_then_forwards_both_ways() {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let child = Command::new(env!("CARGO_BIN_EXE_prisme"))
        .arg("--agent-attach")
        .arg(format!("127.0.0.1:{port}"))
        .env("PRISME_AGENT_SECRET", SECRET)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the attach client");
    let mut child = Attached(child);

    let (stream, _) = listener.accept().expect("the client connects");
    stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    let mut wire = BufReader::new(stream.try_clone().expect("clone"));
    let mut out = &stream;

    // First line, before anything else: the secret it was given, and nothing
    // of the protocol yet.
    let mut first = String::new();
    wire.read_line(&mut first).expect("the first line");
    assert_eq!(first.trim_end(), SECRET);

    // What the application says unprompted reaches the assistant's stdout.
    writeln!(out, r#"{{"ok":true,"event":"ready","frame":0}}"#).expect("greet");
    out.flush().expect("flush");
    let mut stdout = BufReader::new(child.0.stdout.take().expect("stdout"));
    let mut said = String::new();
    stdout.read_line(&mut said).expect("read the greeting back");
    assert!(said.contains("\"event\":\"ready\""), "{said}");

    // …and what the assistant types reaches the application, verbatim.
    let stdin = child.0.stdin.as_mut().expect("stdin");
    writeln!(stdin, r#"{{"id":1,"cmd":"step","frames":300}}"#).expect("send a request");
    stdin.flush().expect("flush");
    let mut request = String::new();
    wire.read_line(&mut request).expect("the request");
    assert_eq!(request.trim_end(), r#"{"id":1,"cmd":"step","frames":300}"#);

    // The answer that comes 300 frames later finds its way back.
    writeln!(out, r#"{{"ok":true,"id":1,"frame":300,"frames":300}}"#).expect("answer");
    out.flush().expect("flush");
    let mut answer = String::new();
    stdout.read_line(&mut answer).expect("read the answer");
    assert!(answer.contains("\"frame\":300"), "{answer}");

    // Closing the channel ends the client, which is how the application knows
    // the assistant is done with the player's console.
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.0.try_wait().expect("wait") {
            Some(status) => {
                assert!(status.success(), "{status}");
                break;
            }
            None if Instant::now() > deadline => panic!("the client outlived the channel"),
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Nothing about this channel is meant to leave the machine, and a client with
/// no secret must not be able to open one at all.
#[test]
fn the_attach_client_refuses_a_non_loopback_address_and_a_missing_secret() {
    let out = Command::new(env!("CARGO_BIN_EXE_prisme"))
        .arg("--agent-attach")
        .arg("10.0.0.1:50000")
        .arg("--agent-secret")
        .arg(SECRET)
        .output()
        .expect("run");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("loopback"));

    let out = Command::new(env!("CARGO_BIN_EXE_prisme"))
        .arg("--agent-attach")
        .arg("127.0.0.1:1")
        .env_remove("PRISME_AGENT_SECRET")
        .output()
        .expect("run");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no secret"));
}
