//! The end of an MCP sign-in for an agent on another machine.
//!
//! Once the user approves, the server sends the browser to `http://localhost:<port>/callback?…`.
//! Claude Code listens on that port on its own machine, but the browser runs on this computer. The
//! app listens on the same port here, on the loopback address only, and hands the address over to
//! Claude Code through the daemon.
//!
//! Any user of this computer can reach the port. The listener takes only an address whose `state`
//! is the one of the sign-in link, as Claude Code does, so a made-up request cannot end the
//! sign-in.

use std::io;
use std::net::Ipv4Addr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

use crate::state::mcp_sign_in;

/// The path Claude Code's redirect address names.
const CALLBACK_PATH: &str = "/callback";
/// The most bytes of a request's head the listener reads.
const MAX_HEAD_BYTES: usize = 8 * 1024;
/// How long a connection may take to send its request.
const HEAD_WAIT: Duration = Duration::from_secs(5);

const DONE_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>Signed in</title></head><body style="font-family: sans-serif; margin: 3em"><p>The sign-in reached the app. You can close this tab.</p></body></html>"#;

const OTHER_SIGN_IN_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>Another sign-in</title></head><body style="font-family: sans-serif; margin: 3em"><p>This address belongs to another sign-in. The app still waits for the one it opened.</p></body></html>"#;

/// Listens on this computer's loopback address, on the port the redirect address names.
pub async fn bind(port: u16) -> io::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await
}

/// Answers requests until one asks for the callback path with `state`, when the sign-in link has
/// one, and gives the address it asked for. Each connection is read on its own, so a silent one
/// does not hold back the next.
pub async fn next_callback(
    listener: &TcpListener,
    port: u16,
    state: Option<&str>,
) -> io::Result<String> {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                connections.spawn(answer(stream, port, state.map(str::to_owned)));
            }
            Some(answered) = connections.join_next() => {
                if let Ok(Some(address)) = answered {
                    return Ok(address);
                }
            }
        }
    }
}

/// Answers one connection, and gives the address it brought when it is the sign-in's.
async fn answer(mut stream: TcpStream, port: u16, state: Option<String>) -> Option<String> {
    let target = match tokio::time::timeout(HEAD_WAIT, read_target(&mut stream)).await {
        Ok(Ok(Some(target))) => target,
        Ok(Ok(None)) => {
            reply(&mut stream, "404 Not Found", "").await;
            return None;
        }
        Ok(Err(error)) => {
            tracing::debug!(%error, "unreadable request on the sign-in listener");
            return None;
        }
        Err(_) => return None,
    };
    if !is_callback(&target) {
        reply(&mut stream, "404 Not Found", "").await;
        return None;
    }
    let address = format!("http://localhost:{port}{target}");
    if state.is_some() && mcp_sign_in::state_of(&address) != state {
        reply(&mut stream, "400 Bad Request", OTHER_SIGN_IN_PAGE).await;
        return None;
    }
    reply(&mut stream, "200 OK", DONE_PAGE).await;
    Some(address)
}

/// The target of a `GET` request, such as `/callback?code=…`, or `None` for another request.
async fn read_target(stream: &mut TcpStream) -> io::Result<Option<String>> {
    let mut head = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") && head.len() < MAX_HEAD_BYTES {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(buffer.get(..read).unwrap_or_default());
    }
    Ok(target_of(&String::from_utf8_lossy(&head)))
}

fn target_of(head: &str) -> Option<String> {
    let line = head.lines().next()?;
    let mut words = line.split(' ');
    let (Some("GET"), Some(target)) = (words.next(), words.next()) else {
        return None;
    };
    Some(target.to_owned())
}

fn is_callback(target: &str) -> bool {
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    path == CALLBACK_PATH
}

async fn reply(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    if let Err(error) = stream.write_all(response.as_bytes()).await {
        tracing::debug!(%error, "cannot answer the browser on the sign-in listener");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test code: a failure is a failed test")]

    use tokio::task::JoinHandle;

    use super::*;

    const CALLBACK_REQUEST: &str =
        "GET /callback?code=c1&state=s1 HTTP/1.1\r\nHost: localhost\r\n\r\n";

    async fn send(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.unwrap();
        answer
    }

    /// A listener on a free port that waits for the sign-in whose state is `s1`.
    async fn listening() -> (u16, JoinHandle<io::Result<String>>) {
        let listener = bind(0).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let waiting = tokio::spawn(async move { next_callback(&listener, port, Some("s1")).await });
        (port, waiting)
    }

    #[tokio::test]
    async fn the_callback_comes_back_whole_after_other_requests() {
        let (port, waiting) = listening().await;

        let icon = send(port, "GET /favicon.ico HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
        let callback = send(port, CALLBACK_REQUEST).await;

        assert!(icon.starts_with("HTTP/1.1 404"), "{icon}");
        assert!(callback.starts_with("HTTP/1.1 200"), "{callback}");
        assert_eq!(
            waiting.await.unwrap().unwrap(),
            format!("http://localhost:{port}/callback?code=c1&state=s1")
        );
    }

    #[tokio::test]
    async fn a_callback_of_another_sign_in_leaves_the_listener_waiting() {
        let (port, waiting) = listening().await;

        let other = send(
            port,
            "GET /callback?code=c9&state=s9 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        let stateless = send(port, "GET /callback?code=c9 HTTP/1.1\r\n\r\n").await;
        send(port, CALLBACK_REQUEST).await;

        assert!(other.starts_with("HTTP/1.1 400"), "{other}");
        assert!(stateless.starts_with("HTTP/1.1 400"), "{stateless}");
        assert!(waiting.await.unwrap().unwrap().contains("code=c1"));
    }

    #[tokio::test]
    async fn a_silent_connection_does_not_hold_back_the_callback() {
        let (port, waiting) = listening().await;
        let _silent = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();

        send(port, CALLBACK_REQUEST).await;

        let address = tokio::time::timeout(Duration::from_secs(2), waiting).await;
        assert!(
            address.is_ok(),
            "the callback waited for the silent connection"
        );
    }

    #[test]
    fn only_a_get_names_a_target() {
        assert_eq!(
            target_of("GET /callback?code=c1 HTTP/1.1\r\n"),
            Some("/callback?code=c1".to_owned())
        );
        assert_eq!(target_of("POST /callback HTTP/1.1\r\n"), None);
        assert!(!is_callback("/callbacks?code=c1"));
        assert!(is_callback("/callback"));
    }
}
