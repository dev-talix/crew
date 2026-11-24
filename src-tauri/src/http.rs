//! The small piece of HTTP/1.1 Grimoire's own listeners need: one request per connection, a
//! Content-Length body, and a reply that closes the connection. Webhooks and linked Macs share it.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const HEAD_LIMIT: usize = 8 * 1024;

#[derive(Debug, PartialEq)]
pub struct Head {
    pub method: String,
    pub path: String,
    /// What follows `?` in the target, without it.
    pub query: String,
    pub length: Option<usize>,
    pub chunked: bool,
    pub authorization: Option<String>,
    pub host: Option<String>,
    pub content_type: Option<String>,
}

pub fn parse_head(head: &str) -> Option<Head> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target, version) = (first.next()?, first.next()?, first.next()?);
    if first.next().is_some() || !version.starts_with("HTTP/1.") {
        return None;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut head = Head {
        method: method.into(),
        path: path.into(),
        query: query.into(),
        length: None,
        chunked: false,
        authorization: None,
        host: None,
        content_type: None,
    };
    for line in lines.filter(|l| !l.is_empty()) {
        let (name, value) = line.split_once(':')?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            head.length = Some(value.parse().ok()?);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            head.chunked = !value.eq_ignore_ascii_case("identity");
        } else if name.eq_ignore_ascii_case("authorization") {
            head.authorization = Some(value.into());
        } else if name.eq_ignore_ascii_case("host") {
            head.host = Some(value.into());
        } else if name.eq_ignore_ascii_case("content-type") {
            head.content_type = Some(value.into());
        }
    }
    Some(head)
}

/// The value of `key` in a query string like `after=3&wait=25`.
pub fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| pair.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
}

/// Compares secrets in time that doesn't depend on where they differ.
pub fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub struct Request {
    pub head: Head,
    pub body: Vec<u8>,
}

/// Why a request couldn't be read: a status and text to answer with, or None when the other side
/// went away and there's nobody to answer.
pub type Refusal = Option<(&'static str, &'static str)>;

/// Reads one request. `body_ok` sees the head first and can refuse before any body is read.
pub async fn read_request<S: AsyncRead + Unpin>(
    stream: &mut S,
    body_limit: usize,
    body_ok: impl FnOnce(&Head) -> Result<(), (&'static str, &'static str)>,
) -> Result<Request, Refusal> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buf.len() > HEAD_LIMIT {
            return Err(Some(("431 Request Header Fields Too Large", "Headers too large.")));
        }
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return Err(None),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let Some(head) = std::str::from_utf8(&buf[..end]).ok().and_then(parse_head) else {
        return Err(Some(("400 Bad Request", "Bad request.")));
    };
    body_ok(&head).map_err(Some)?;
    if head.chunked {
        return Err(Some(("411 Length Required", "Send a Content-Length.")));
    }
    let length = head.length.unwrap_or(0);
    if length > body_limit {
        return Err(Some(("413 Payload Too Large", "The body is too large.")));
    }
    let mut body = buf[end + 4..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return Err(None),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(length);
    Ok(Request { head, body })
}

/// Writes a whole response and closes the connection.
pub async fn reply<S: AsyncWrite + Unpin>(stream: &mut S, status: &str, content_type: &str, body: &[u8]) {
    reply_with(stream, status, content_type, &[], body).await
}

/// `reply` with more headers, each a name and a value.
pub async fn reply_with<S: AsyncWrite + Unpin>(stream: &mut S, status: &str, content_type: &str, headers: &[(&str, &str)], body: &[u8]) {
    let extra: String = headers.iter().map(|(name, value)| format!("{name}: {value}\r\n")).collect();
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n", body.len());
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_heads() {
        let head = parse_head("POST /hooks/abc?x=1 HTTP/1.1\r\nHost: 127.0.0.1\r\ncontent-length: 12\r\nAuthorization: Bearer t").unwrap();
        assert_eq!(
            head,
            Head {
                method: "POST".into(),
                path: "/hooks/abc".into(),
                query: "x=1".into(),
                length: Some(12),
                chunked: false,
                authorization: Some("Bearer t".into()),
                host: Some("127.0.0.1".into()),
                content_type: None,
            }
        );
        assert!(parse_head("POST /hooks/abc HTTP/1.1\r\nTransfer-Encoding: chunked").unwrap().chunked);
        assert_eq!(parse_head("GET / HTTP/1.0").unwrap().length, None);
        for bad in ["", "POST /x", "POST /x SPDY/3", "POST /x HTTP/1.1 extra", "POST /x HTTP/1.1\r\nbroken", "POST /x HTTP/1.1\r\nContent-Length: -1"] {
            assert!(parse_head(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn reads_query_params() {
        assert_eq!(query_param("after=3&wait=25", "wait"), Some("25"));
        assert_eq!(query_param("after=3", "wait"), None);
        assert_eq!(query_param("", "after"), None);
    }

    #[tokio::test]
    async fn reads_a_body_up_to_its_length_and_refuses_big_ones() {
        let mut input: &[u8] = b"POST /x HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello extra";
        let request = read_request(&mut input, 10, |_| Ok(())).await.ok().unwrap();
        assert_eq!(request.body, b"hello");
        let mut big: &[u8] = b"POST /x HTTP/1.1\r\nContent-Length: 50\r\n\r\n";
        let refused = read_request(&mut big, 10, |_| Ok(())).await.err().unwrap();
        assert_eq!(refused.unwrap().0, "413 Payload Too Large");
    }
}
