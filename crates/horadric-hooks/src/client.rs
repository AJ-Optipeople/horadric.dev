//! The sending side, for the command line: one short HTTP POST to the
//! running app.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Posts a JSON body to the listener on localhost and returns the HTTP status.
/// Connection refused means nothing is listening, reported as the error.
pub fn post(port: u16, path: &str, headers: &[(&str, &str)], body: &str) -> io::Result<u16> {
    ask(port, path, headers, body, Duration::from_secs(2)).map(|(status, _)| status)
}

/// Posts like [`post`] and waits up to `wait` for the reply, whose status
/// and body it returns.
pub fn ask(
    port: u16,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
    wait: Duration,
) -> io::Result<(u16, String)> {
    let mut stream =
        TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(300))?;
    stream.set_read_timeout(Some(wait))?;
    let mut req = format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    for (name, value) in headers {
        req.push_str(&format!("{name}: {value}\r\n"));
    }
    req.push_str(&format!(
        "Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    stream.write_all(req.as_bytes())?;

    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    let code = status(&reply)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not an HTTP reply"))?;
    let body = reply
        .split_once("\r\n\r\n")
        .map_or("", |(_, b)| b)
        .to_string();
    Ok((code, body))
}

/// The status code from the first line of a reply.
fn status(reply: &str) -> Option<u16> {
    reply.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_status_code() {
        assert_eq!(
            status("HTTP/1.1 503 Service Unavailable\r\n\r\n"),
            Some(503)
        );
        assert_eq!(status("garbage"), None);
    }
}
