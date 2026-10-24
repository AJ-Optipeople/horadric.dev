//! A small HTTP/1.1 client over [`crate::stream`], shaped like
//! `horadric-ui/src/net.rs` so code that runs on both ports easily: one
//! request per connection, a body read to its `Content-Length`, its
//! chunks, or the connection's end, and redirects followed for a GET only
//! (a model on Hugging Face redirects to its CDN). A large GET streams to
//! a file instead of memory.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::time::Duration;

use horadric_core::release::{split_url, Url};

use crate::stream::Stream;

/// A slow server gives up after a minute rather than hang the thread.
const TIMEOUT: Duration = Duration::from_secs(60);

/// More redirects than this is a loop.
const MAX_REDIRECTS: usize = 10;

/// A response head larger than this is not one we want.
const MAX_HEAD: usize = 64 * 1024;

/// What a server answered, whatever its status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u32,
    /// The `Location` header, which a resumable upload's address comes in.
    pub location: Option<String>,
    pub body: Vec<u8>,
}

/// The body of `url`, which must answer 200 and send at most `limit`
/// bytes.
pub fn get(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let answer = request("GET", url, &[], &[], limit)?;
    if answer.status != 200 {
        return Err(format!("{url} answered {}", answer.status));
    }
    Ok(answer.body)
}

/// POSTs `form`, already `application/x-www-form-urlencoded`, as WorkOS's
/// endpoints take it.
pub fn post_form(url: &str, form: &str, limit: usize) -> Result<Answer, String> {
    request(
        "POST",
        url,
        &[("Content-Type", "application/x-www-form-urlencoded")],
        form.as_bytes(),
        limit,
    )
}

/// Sends `method` to `url` with `headers` and `body`, and reads at most
/// `limit` bytes back. Only a GET follows redirects, as in `net.rs`.
pub fn request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    limit: usize,
) -> Result<Answer, String> {
    let (head, mut reader, _) = open(method, url, headers, body)?;
    let mut out = Vec::new();
    (&mut reader)
        .take(limit as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("reading {url}: {e}"))?;
    if out.len() > limit {
        return Err(format!("{url} sent more than {limit} bytes"));
    }
    Ok(Answer {
        status: head.status,
        location: head.header("location").map(str::to_string),
        body: out,
    })
}

/// GETs `url` into the file `to`, at most `limit` bytes, calling
/// `progress` with the bytes so far and the whole size when the server
/// said it. The body lands in `to` with `.part` added and is renamed only
/// once whole, so a broken download never looks finished. Returns its
/// size.
pub fn download(
    url: &str,
    to: &Path,
    limit: u64,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<u64, String> {
    let (head, mut reader, length) = open("GET", url, &[], &[])?;
    if head.status != 200 {
        return Err(format!("{url} answered {}", head.status));
    }
    let total = match length {
        Length::Fixed(n) if n > limit => {
            return Err(format!("{url} is {n} bytes, more than {limit}"));
        }
        Length::Fixed(n) => Some(n),
        _ => None,
    };
    let part = {
        let mut name = to.as_os_str().to_owned();
        name.push(".part");
        std::path::PathBuf::from(name)
    };
    let written = (|| {
        let mut file = File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        let mut buf = vec![0u8; 256 * 1024];
        let mut done = 0u64;
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| format!("reading {url}: {e}"))?;
            if n == 0 {
                break;
            }
            done += n as u64;
            if done > limit {
                return Err(format!("{url} sent more than {limit} bytes"));
            }
            file.write_all(&buf[..n])
                .map_err(|e| format!("{}: {e}", part.display()))?;
            progress(done, total);
        }
        file.sync_all()
            .map_err(|e| format!("{}: {e}", part.display()))?;
        Ok(done)
    })();
    match written {
        Ok(n) => {
            std::fs::rename(&part, to).map_err(|e| format!("{}: {e}", to.display()))?;
            Ok(n)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Sends the request, follows a GET's redirects, and hands back the final
/// head with a reader of just its body.
fn open(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(Head, Box<dyn Read>, Length), String> {
    let mut url = url.to_string();
    let mut parts = parse(&url)?;
    let first = parts.host.clone();
    let mut hops = 0;
    loop {
        let mut stream = Stream::connect(&parts.host, parts.port, parts.secure)?;
        stream.set_timeouts(Some(TIMEOUT), Some(TIMEOUT));
        stream
            .handshake()
            .map_err(|e| format!("TLS with {}: {e}", parts.host))?;
        let carried = carried(headers, parts.host != first);
        let request = request_head(method, &parts, &carried, body.len());
        stream
            .write_all(request.as_bytes())
            .and_then(|_| stream.write_all(body))
            .and_then(|_| stream.flush())
            .map_err(|e| format!("cannot send to {}: {e}", parts.host))?;
        let mut reader = BufReader::new(stream);
        let head =
            read_head(&mut reader).map_err(|e| format!("{} answered badly: {e}", parts.host))?;
        let redirect = method == "GET" && matches!(head.status, 301 | 302 | 303 | 307 | 308);
        if redirect {
            let location = head
                .header("location")
                .ok_or_else(|| format!("{url} redirected nowhere"))?;
            let next = resolve(&parts, location)
                .ok_or_else(|| format!("{url} redirected to `{location}`"))?;
            hops += 1;
            if hops > MAX_REDIRECTS {
                return Err(format!("{url} redirected more than {MAX_REDIRECTS} times"));
            }
            let next_parts = parse(&next)?;
            if parts.secure && !next_parts.secure {
                return Err(format!("{url} redirected to plain http"));
            }
            (url, parts) = (next, next_parts);
            continue;
        }
        let length = length(&head, method).map_err(|e| format!("{url}: {e}"))?;
        let body: Box<dyn Read> = match length {
            Length::Empty => Box::new(io::empty()),
            Length::Fixed(n) => Box::new(Exact {
                inner: reader,
                left: n,
            }),
            Length::Chunked => Box::new(Chunked::new(reader)),
            Length::ToEnd => Box::new(reader),
        };
        return Ok((head, body, length));
    }
}

/// The headers a request sends, once redirected `elsewhere` without
/// what was meant for the first host only: a token above all. A `Range`
/// goes on, so part of a model still comes from its CDN.
fn carried<'a>(headers: &[(&'a str, &'a str)], elsewhere: bool) -> Vec<(&'a str, &'a str)> {
    headers
        .iter()
        .filter(|(k, _)| {
            !elsewhere
                || !(k.eq_ignore_ascii_case("authorization") || k.eq_ignore_ascii_case("cookie"))
        })
        .copied()
        .collect()
}

fn parse(url: &str) -> Result<Url, String> {
    split_url(url).ok_or_else(|| format!("`{url}` is not an http or https URL"))
}

/// The request line and headers. `Connection: close`, since every
/// request has a connection of its own, and no `Accept-Encoding`, so the
/// body comes as it is.
fn request_head(method: &str, url: &Url, headers: &[(&str, &str)], body: usize) -> String {
    let default = if url.secure { 443 } else { 80 };
    let host = if url.port == default {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Horadric/{}\r\nAccept: */*\r\nConnection: close\r\n",
        url.path,
        env!("CARGO_PKG_VERSION")
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if body > 0 || !matches!(method, "GET" | "HEAD" | "DELETE") {
        head.push_str(&format!("Content-Length: {body}\r\n"));
    }
    head.push_str("\r\n");
    head
}

/// A response's status and headers, names in lower case.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Head {
    status: u32,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Reads heads until a final one: a `100 Continue` or a `103 Early
/// Hints` comes before the real answer and is skipped.
fn read_head(r: &mut impl BufRead) -> io::Result<Head> {
    loop {
        let mut text = Vec::new();
        loop {
            let n = r.read_until(b'\n', &mut text)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the connection closed before an answer",
                ));
            }
            if text.len() > MAX_HEAD {
                return Err(io::Error::other("an answer's head is too long"));
            }
            if text.ends_with(b"\r\n\r\n") || text.ends_with(b"\n\n") {
                break;
            }
        }
        let head = parse_head(&String::from_utf8_lossy(&text))
            .ok_or_else(|| io::Error::other("not an HTTP answer"))?;
        if !(100..200).contains(&head.status) || head.status == 101 {
            return Ok(head);
        }
    }
}

fn parse_head(text: &str) -> Option<Head> {
    let mut lines = text.split('\n').map(|l| l.trim_end_matches('\r'));
    let status = parse_status(lines.next()?)?;
    let headers = lines
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    Some(Head { status, headers })
}

/// The code in `HTTP/1.1 200 OK`.
fn parse_status(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("HTTP/1.")?;
    let mut words = rest.split(' ');
    let _minor = words.next()?;
    let code = words.next()?;
    (code.len() == 3)
        .then(|| code.parse().ok())
        .flatten()
        .filter(|c| (100..600).contains(c))
}

/// How the body's end is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Length {
    Empty,
    Fixed(u64),
    Chunked,
    /// Neither said: the body is all there is until the server closes.
    ToEnd,
}

fn length(head: &Head, method: &str) -> Result<Length, String> {
    if method == "HEAD" || matches!(head.status, 100..=199 | 204 | 304) {
        return Ok(Length::Empty);
    }
    if let Some(te) = head.header("transfer-encoding") {
        // Only chunked is ever sent to a client that asked for nothing
        // else; anything stacked on it, the body could not be read.
        if te.eq_ignore_ascii_case("chunked") {
            return Ok(Length::Chunked);
        }
        return Err(format!("a body in `{te}` cannot be read"));
    }
    match head.header("content-length") {
        Some(n) => n
            .parse()
            .map(Length::Fixed)
            .map_err(|_| format!("a Content-Length of `{n}`")),
        None => Ok(Length::ToEnd),
    }
}

/// Where a redirect from `base` to `location` goes, `None` for one that
/// is not http or https.
fn resolve(base: &Url, location: &str) -> Option<String> {
    let scheme = if base.secure { "https" } else { "http" };
    if location.starts_with("https://") || location.starts_with("http://") {
        return Some(location.to_string());
    }
    if location.contains("://") {
        return None;
    }
    if let Some(rest) = location.strip_prefix("//") {
        return Some(format!("{scheme}://{rest}"));
    }
    let default = if base.secure { 443 } else { 80 };
    let origin = if base.port == default {
        format!("{scheme}://{}", base.host)
    } else {
        format!("{scheme}://{}:{}", base.host, base.port)
    };
    if location.starts_with('/') {
        return Some(format!("{origin}{location}"));
    }
    let path = base.path.split(['?', '#']).next().unwrap_or("/");
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    Some(format!("{origin}{dir}{location}"))
}

/// A body of a known length: reading past it would wait on a connection
/// the server may hold open, and stopping short of it is an error.
struct Exact<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Exact<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len().min(self.left.min(usize::MAX as u64) as usize);
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("the body ended {} bytes short", self.left),
            ));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

/// A chunked body, read chunk by chunk, the trailer skipped.
struct Chunked<R> {
    inner: R,
    /// Bytes left in this chunk.
    left: u64,
    done: bool,
}

impl<R: BufRead> Chunked<R> {
    fn new(inner: R) -> Chunked<R> {
        Chunked {
            inner,
            left: 0,
            done: false,
        }
    }

    fn line(&mut self) -> io::Result<String> {
        let mut line = Vec::new();
        let n = (&mut self.inner).take(4096).read_until(b'\n', &mut line)?;
        if n == 0 || !line.ends_with(b"\n") {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "a chunked body ended early",
            ));
        }
        Ok(String::from_utf8_lossy(&line).trim_end().to_string())
    }
}

/// A chunk's size line, `1a3f` or `1a3f;name=value`.
fn chunk_size(line: &str) -> Option<u64> {
    let hex = line.split(';').next()?.trim();
    if hex.is_empty() || hex.len() > 16 {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let line = self.line()?;
            let size = chunk_size(&line)
                .ok_or_else(|| io::Error::other(format!("a chunk size of `{line}`")))?;
            if size == 0 {
                // The trailer, header lines until an empty one.
                while !self.line()?.is_empty() {}
                self.done = true;
                return Ok(0);
            }
            self.left = size;
        }
        let want = buf.len().min(self.left.min(usize::MAX as u64) as usize);
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "a chunk ended early",
            ));
        }
        self.left -= n as u64;
        if self.left == 0 && !self.line()?.is_empty() {
            return Err(io::Error::other("a chunk ran past its size"));
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn url(s: &str) -> Url {
        split_url(s).unwrap()
    }

    #[test]
    fn a_request_says_its_host_and_length() {
        let head = request_head(
            "POST",
            &url("https://api.workos.com/user_management/authenticate"),
            &[("Content-Type", "application/x-www-form-urlencoded")],
            12,
        );
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            head,
            format!(
                "POST /user_management/authenticate HTTP/1.1\r\nHost: api.workos.com\r\n\
                 User-Agent: Horadric/{version}\r\nAccept: */*\r\nConnection: close\r\n\
                 Content-Type: application/x-www-form-urlencoded\r\nContent-Length: 12\r\n\r\n"
            )
        );
        let get = request_head("GET", &url("http://127.0.0.1:8080/a?b=c"), &[], 0);
        assert!(get.starts_with("GET /a?b=c HTTP/1.1\r\nHost: 127.0.0.1:8080\r\n"));
        assert!(!get.contains("Content-Length"));
        let empty_post = request_head("POST", &url("https://x.dev"), &[], 0);
        assert!(empty_post.starts_with("POST / HTTP/1.1\r\nHost: x.dev\r\n"));
        assert!(empty_post.contains("Content-Length: 0\r\n"));
    }

    #[test]
    fn a_token_stays_with_its_host() {
        let headers = [
            ("Authorization", "Bearer t"),
            ("cookie", "c"),
            ("Range", "bytes=0-9"),
        ];
        assert_eq!(carried(&headers, false), headers.to_vec());
        assert_eq!(carried(&headers, true), vec![("Range", "bytes=0-9")]);
    }

    #[test]
    fn status_lines_read() {
        assert_eq!(parse_status("HTTP/1.1 200 OK"), Some(200));
        assert_eq!(parse_status("HTTP/1.0 404 Not Found"), Some(404));
        assert_eq!(parse_status("HTTP/1.1 302"), Some(302));
        assert_eq!(parse_status("HTTP/2 200"), None);
        assert_eq!(parse_status("HTTP/1.1 20 OK"), None);
        assert_eq!(parse_status("HTTP/1.1 999 Odd"), None);
        assert_eq!(parse_status("SSH-2.0-OpenSSH"), None);
    }

    #[test]
    fn heads_read_with_names_in_lower_case() {
        let head = parse_head(
            "HTTP/1.1 302 Found\r\nLocation: /api/x\r\nContent-Length:  0 \r\nX-Odd\r\n\r\n",
        )
        .unwrap();
        assert_eq!(head.status, 302);
        assert_eq!(head.header("location"), Some("/api/x"));
        assert_eq!(head.header("content-length"), Some("0"));
        assert_eq!(head.header("x-odd"), None);
    }

    #[test]
    fn interim_answers_are_skipped() {
        let mut r = Cursor::new(
            b"HTTP/1.1 103 Early Hints\r\nLink: </a>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi"
                .to_vec(),
        );
        let head = read_head(&mut r).unwrap();
        assert_eq!(head.status, 200);
        let mut rest = String::new();
        r.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "hi");
        assert!(read_head(&mut Cursor::new(b"HTTP/1.1 200 OK\r\n".to_vec())).is_err());
        assert!(read_head(&mut Cursor::new(b"garbage\r\n\r\n".to_vec())).is_err());
    }

    #[test]
    fn the_body_ends_where_the_head_says() {
        let head = |s: &str| parse_head(s).unwrap();
        assert_eq!(
            length(&head("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n"), "GET"),
            Ok(Length::Fixed(5))
        );
        assert_eq!(
            length(
                &head("HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\nContent-Length: 5\r\n"),
                "GET"
            ),
            Ok(Length::Chunked)
        );
        assert_eq!(
            length(&head("HTTP/1.1 200 OK\r\n"), "GET"),
            Ok(Length::ToEnd)
        );
        assert_eq!(
            length(&head("HTTP/1.1 204 No Content\r\n"), "DELETE"),
            Ok(Length::Empty)
        );
        assert_eq!(
            length(&head("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n"), "HEAD"),
            Ok(Length::Empty)
        );
        assert!(length(
            &head("HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n"),
            "GET"
        )
        .is_err());
        assert!(length(&head("HTTP/1.1 200 OK\r\nContent-Length: -1\r\n"), "GET").is_err());
    }

    #[test]
    fn redirects_resolve_against_where_they_came_from() {
        let base = url("https://huggingface.co/a/b/resolve/main/model.safetensors?x=1");
        assert_eq!(
            resolve(&base, "https://cdn-lfs.hf.co/x?sig=1").as_deref(),
            Some("https://cdn-lfs.hf.co/x?sig=1")
        );
        assert_eq!(
            resolve(&base, "/api/resolve-cache/x").as_deref(),
            Some("https://huggingface.co/api/resolve-cache/x")
        );
        assert_eq!(
            resolve(&base, "//cdn.example/y").as_deref(),
            Some("https://cdn.example/y")
        );
        assert_eq!(
            resolve(&base, "other.json").as_deref(),
            Some("https://huggingface.co/a/b/resolve/main/other.json")
        );
        assert_eq!(
            resolve(&url("http://127.0.0.1:81/x"), "/y").as_deref(),
            Some("http://127.0.0.1:81/y")
        );
        assert_eq!(resolve(&base, "ftp://x/y"), None);
    }

    #[test]
    fn an_exact_body_stops_at_its_length_and_minds_a_short_one() {
        let mut out = Vec::new();
        Exact {
            inner: Cursor::new(b"hello world".to_vec()),
            left: 5,
        }
        .read_to_end(&mut out)
        .unwrap();
        assert_eq!(out, b"hello");
        let short = Exact {
            inner: Cursor::new(b"hel".to_vec()),
            left: 5,
        }
        .read_to_end(&mut Vec::new());
        assert!(short.is_err());
    }

    #[test]
    fn chunk_sizes_read() {
        assert_eq!(chunk_size("1a"), Some(26));
        assert_eq!(chunk_size("1A;name=v"), Some(26));
        assert_eq!(chunk_size("0"), Some(0));
        assert_eq!(chunk_size(""), None);
        assert_eq!(chunk_size("zz"), None);
        assert_eq!(chunk_size("11111111111111111"), None);
    }

    fn unchunk(body: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        Chunked::new(Cursor::new(body.to_vec())).read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn chunked_bodies_join_and_their_trailer_goes() {
        assert_eq!(
            unchunk(b"4\r\nWiki\r\n6;x=y\r\npedia \r\nE\r\nin \r\n\r\nchunks.\r\n0\r\nX-T: 1\r\n\r\nafter")
                .unwrap(),
            b"Wikipedia in \r\n\r\nchunks."
        );
        assert_eq!(unchunk(b"0\r\n\r\n").unwrap(), b"");
        assert!(unchunk(b"4\r\nWik").is_err());
        assert!(unchunk(b"4\r\nWikipedia\r\n0\r\n\r\n").is_err());
        assert!(unchunk(b"4\r\nWiki\r\n").is_err());
        assert!(unchunk(b"q\r\n").is_err());
    }

    /// Against the real network: WorkOS answers a form POST, and a small
    /// Hugging Face file comes through its redirect. Run by hand with
    /// `cargo test -p horadric-link -- --ignored`.
    #[test]
    #[ignore]
    fn reaches_hugging_face_and_workos() {
        let url = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/config.json";
        let body = get(url, 1 << 20).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v.get("hidden_size").is_some(), "{v}");
        let dir = std::env::temp_dir().join(format!("horadric-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let to = dir.join("config.json");
        let mut seen = 0;
        let n = download(url, &to, 1 << 20, &mut |done, _| seen = done).unwrap();
        assert_eq!((n, seen), (body.len() as u64, body.len() as u64));
        assert_eq!(std::fs::read(&to).unwrap(), body);
        assert!(download(url, &to, 10, &mut |_, _| {}).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
        let missing = request(
            "GET",
            "https://huggingface.co/no/such/resolve/main/x",
            &[],
            &[],
            1 << 16,
        )
        .unwrap();
        assert!(missing.status >= 400, "{}", missing.status);
        // A model's weights redirect to another host, the CDN.
        let weights = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/model.safetensors";
        let part = request("GET", weights, &[("Range", "bytes=0-1023")], &[], 4096).unwrap();
        assert_eq!((part.status, part.body.len()), (206, 1024));
        // The device authorization WorkOS's CLI sign in starts with.
        let client = horadric_core::workos::DEV_CLIENT_ID;
        let answer = post_form(
            horadric_core::workos::DEVICE_URL,
            &horadric_core::workos::device_body(client),
            1 << 16,
        )
        .unwrap();
        let device = horadric_core::workos::parse_device(&answer.body)
            .unwrap_or_else(|e| panic!("{} {e}", answer.status));
        let poll = post_form(
            horadric_core::workos::AUTHENTICATE_URL,
            &horadric_core::workos::poll_body(client, &device.device_code),
            1 << 16,
        )
        .unwrap();
        assert_eq!(
            horadric_core::workos::parse_poll(&poll.body),
            horadric_core::workos::Poll::Pending,
            "{}",
            String::from_utf8_lossy(&poll.body)
        );
    }
}
