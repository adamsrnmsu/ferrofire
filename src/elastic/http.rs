//! The smallest HTTP/1.1 client that talks to Elasticsearch and OpenSearch: one request per connection (`Connection: close`),
//! plain TCP or rustls (ring), `Content-Length` and chunked responses. Roots come from a CA file or the operating system.
//! ponytail: no keep-alive and no proxy support; one TLS handshake per bulk request is cheap at a batch per second.

use super::{Auth, Config};
use base64::{engine::general_purpose::STANDARD, Engine};
use rustls::pki_types::{pem::PemObject, CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

const CONNECT: Duration = Duration::from_secs(10);
const IO: Duration = Duration::from_secs(30);
/// A response larger than this is an error, not a reason to run out of memory.
const MAX_BODY: usize = 32 << 20;

#[derive(Debug, PartialEq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    /// `""` or `/path` with no trailing slash: put in front of every request path.
    pub prefix: String,
}

/// `(https, rest)` of a URL with an `http://` or `https://` scheme.
fn scheme(u: &str) -> Result<(bool, &str), String> {
    match (u.strip_prefix("https://"), u.strip_prefix("http://")) {
        (Some(r), _) => Ok((true, r)),
        (_, Some(r)) => Ok((false, r)),
        _ => Err(format!(
            "`{u}`: expected http://host:port or https://host:port"
        )),
    }
}

fn host_port(u: &str, hostport: &str, https: bool) -> Result<(String, u16), String> {
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| format!("`{u}`: bad port `{p}`"))?),
        None => (hostport, if https { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(format!("`{u}`: no host"));
    }
    Ok((host.into(), port))
}

pub fn parse_url(u: &str) -> Result<Url, String> {
    let (https, rest) = scheme(u)?;
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = host_port(u, hostport, https)?;
    let path = path.trim_end_matches('/');
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("/{path}")
    };
    Ok(Url {
        https,
        host,
        port,
        prefix,
    })
}

/// The `Authorization` header value, if any.
pub fn auth_header(a: &Auth) -> Option<String> {
    match a {
        Auth::None => None,
        Auth::Basic(u, p) => Some(format!("Basic {}", STANDARD.encode(format!("{u}:{p}")))),
        Auth::ApiKey(k) => Some(format!("ApiKey {k}")),
    }
}

/// The request bytes: head and body.
pub fn encode_request(
    method: &str,
    u: &Url,
    path: &str,
    auth: Option<&str>,
    ctype: &str,
    body: &[u8],
) -> Vec<u8> {
    let auth = auth.map_or(String::new(), |a| format!("Authorization: {a}\r\n"));
    let mut r = format!(
        "{method} {}{path} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: ferrofire\r\nAccept: application/json\r\nConnection: close\r\n{auth}Content-Type: {ctype}\r\nContent-Length: {}\r\n\r\n",
        u.prefix,
        u.host,
        u.port,
        body.len()
    )
    .into_bytes();
    r.extend_from_slice(body);
    r
}

fn line(r: &mut impl BufRead) -> Result<String, String> {
    let mut l = String::new();
    let n = r.read_line(&mut l).map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("connection closed before the response was complete".into());
    }
    Ok(l.trim_end().to_string())
}

fn exact(r: &mut impl Read, n: usize) -> Result<Vec<u8>, String> {
    if n > MAX_BODY {
        return Err(format!(
            "response of {n} bytes is over the {MAX_BODY} limit"
        ));
    }
    let mut b = vec![0; n];
    r.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(b)
}

fn chunked(r: &mut impl BufRead) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    loop {
        let l = line(r)?;
        let size = l.split(';').next().unwrap_or("").trim();
        let n = usize::from_str_radix(size, 16).map_err(|_| format!("bad chunk size `{size}`"))?;
        if n == 0 {
            return Ok(out); // trailers, if any, are left unread: the connection closes after this
        }
        if out.len() + n > MAX_BODY {
            return Err(format!("response is over the {MAX_BODY} byte limit"));
        }
        out.extend(exact(r, n)?);
        line(r)?; // the CRLF after the chunk
    }
}

fn until_close(r: &mut impl Read) -> Result<Vec<u8>, String> {
    let mut b = vec![];
    // a TLS peer that closes without close_notify reads as UnexpectedEof: that is the end of the body here
    match r.take(MAX_BODY as u64 + 1).read_to_end(&mut b) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        Err(e) => return Err(e.to_string()),
    }
    if b.len() > MAX_BODY {
        return Err(format!("response is over the {MAX_BODY} byte limit"));
    }
    Ok(b)
}

/// `(content-length, chunked)` from the header lines up to the blank line.
fn headers(r: &mut impl BufRead) -> Result<(Option<usize>, bool), String> {
    let (mut len, mut chunks) = (None, false);
    loop {
        let h = line(r)?;
        let Some((k, v)) = h.split_once(':') else {
            return Ok((len, chunks));
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "content-length" => len = v.parse().ok(),
            "transfer-encoding" => chunks = v.to_ascii_lowercase().contains("chunked"),
            _ => {}
        }
    }
}

fn status_code(status: &str) -> Result<u16, String> {
    let code = status.split(' ').nth(1);
    code.and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("bad status line `{status}`"))
}

fn body(r: &mut impl BufRead, framing: (Option<usize>, bool)) -> Result<Vec<u8>, String> {
    match framing {
        (_, true) => chunked(r),
        (Some(n), _) => exact(r, n),
        _ => until_close(r),
    }
}

/// Status and body of one response.
pub fn read_response(r: &mut impl BufRead) -> Result<(u16, Vec<u8>), String> {
    let code = status_code(&line(r)?)?;
    let framing = headers(r)?;
    let body = body(r, framing)?;
    Ok((code, body))
}

pub struct Http {
    url: Url,
    auth: Option<String>,
    tls: Option<Arc<ClientConfig>>,
}

fn ca_file_roots(f: &str) -> Result<RootCertStore, String> {
    let mut store = RootCertStore::empty();
    for c in CertificateDer::pem_file_iter(f).map_err(|e| format!("{f}: {e}"))? {
        store
            .add(c.map_err(|e| format!("{f}: {e}"))?)
            .map_err(|e| format!("{f}: {e}"))?;
    }
    match store.is_empty() {
        true => Err(format!("{f}: no certificates in the CA file")),
        false => Ok(store),
    }
}

fn roots(ca_file: Option<&str>) -> Result<RootCertStore, String> {
    if let Some(f) = ca_file {
        return ca_file_roots(f);
    }
    // native roots; an unreadable one is skipped
    let mut store = RootCertStore::empty();
    let _ = store.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    match store.is_empty() {
        true => Err("no CA certificates in the system store: set the CA file".into()),
        false => Ok(store),
    }
}

fn tls_config(ca_file: Option<&str>) -> Result<Arc<ClientConfig>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let c = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots(ca_file)?)
        .with_no_client_auth();
    Ok(Arc::new(c))
}

impl Http {
    pub fn new(c: &Config) -> Result<Http, String> {
        let url = parse_url(&c.url)?;
        let tls = if url.https {
            Some(tls_config(c.ca_file.as_deref())?)
        } else {
            None
        };
        Ok(Http {
            url,
            auth: auth_header(&c.auth),
            tls,
        })
    }

    /// One request, one response: `(status, body)`. Any transport failure is an `Err` (the caller retries).
    pub fn request(
        &self,
        method: &str,
        path: &str,
        ctype: &str,
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), String> {
        let u = &self.url;
        let tcp = connect(&u.host, u.port)?;
        tcp.set_read_timeout(Some(IO)).map_err(|e| e.to_string())?;
        tcp.set_write_timeout(Some(IO)).map_err(|e| e.to_string())?;
        let req = encode_request(method, u, path, self.auth.as_deref(), ctype, body);
        match &self.tls {
            None => exchange(tcp, &req),
            Some(cfg) => {
                let name =
                    ServerName::try_from(u.host.clone()).map_err(|e| format!("{}: {e}", u.host))?;
                let conn = ClientConnection::new(cfg.clone(), name).map_err(|e| e.to_string())?;
                exchange(StreamOwned::new(conn, tcp), &req)
            }
        }
    }
}

/// Connect to the first address of `host` that answers (`localhost` may resolve to `::1` first while the server listens on IPv4).
fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("{host}:{port}: {e}"))?;
    let mut last = format!("{host}: no address");
    for a in addrs {
        match TcpStream::connect_timeout(&a, CONNECT) {
            Ok(t) => return Ok(t),
            Err(e) => last = format!("{a}: {e}"),
        }
    }
    Err(last)
}

fn exchange<S: Read + Write>(mut s: S, req: &[u8]) -> Result<(u16, Vec<u8>), String> {
    s.write_all(req).map_err(|e| e.to_string())?;
    s.flush().map_err(|e| e.to_string())?;
    read_response(&mut BufReader::new(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let u = parse_url("https://es.example:9243/base/").unwrap();
        assert_eq!(
            u,
            Url {
                https: true,
                host: "es.example".into(),
                port: 9243,
                prefix: "/base".into()
            }
        );
        let u = parse_url("http://10.0.0.5").unwrap();
        assert_eq!((u.https, u.port, u.prefix.as_str()), (false, 80, ""));
        assert_eq!(parse_url("https://h").unwrap().port, 443);
        assert!(parse_url("ftp://h").unwrap_err().contains("expected http"));
        assert!(parse_url("http://h:x").unwrap_err().contains("bad port"));
        assert!(parse_url("http://:9200").unwrap_err().contains("no host"));
    }

    #[test]
    fn auth_headers() {
        assert_eq!(auth_header(&Auth::None), None);
        let b = auth_header(&Auth::Basic("elastic".into(), "pw".into()));
        assert_eq!(b.as_deref(), Some("Basic ZWxhc3RpYzpwdw=="));
        assert_eq!(
            auth_header(&Auth::ApiKey("abc==".into())).as_deref(),
            Some("ApiKey abc==")
        );
    }

    #[test]
    fn request_bytes() {
        let u = parse_url("http://h:9200/p").unwrap();
        let r = encode_request(
            "POST",
            &u,
            "/_bulk",
            Some("ApiKey k"),
            "application/x-ndjson",
            b"{}\n",
        );
        let t = String::from_utf8(r).unwrap();
        assert!(t.starts_with("POST /p/_bulk HTTP/1.1\r\nHost: h:9200\r\n"));
        assert!(t.contains("\r\nAuthorization: ApiKey k\r\n"));
        assert!(
            t.contains("\r\nContent-Type: application/x-ndjson\r\nContent-Length: 3\r\n\r\n{}\n")
        );
        assert!(t.contains("\r\nConnection: close\r\n"));
        let t = String::from_utf8(encode_request(
            "PUT",
            &u,
            "/x",
            None,
            "application/json",
            b"",
        ))
        .unwrap();
        assert!(!t.contains("Authorization"));
        assert!(t.ends_with("Content-Length: 0\r\n\r\n"));
    }

    fn resp(b: &[u8]) -> Result<(u16, Vec<u8>), String> {
        read_response(&mut BufReader::new(b))
    }

    #[test]
    fn responses_by_length_chunks_and_close() {
        let (c, b) =
            resp(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nX-A: b\r\n\r\nbodyEXTRA").unwrap();
        assert_eq!((c, b.as_slice()), (200, &b"body"[..]));
        let (c, b) = resp(b"HTTP/1.1 429 Too Many\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n").unwrap();
        assert_eq!((c, b.as_slice()), (429, &b"abcde"[..]));
        let (c, b) = resp(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nrest of it").unwrap();
        assert_eq!((c, b.as_slice()), (200, &b"rest of it"[..]));
        // header names are case-insensitive
        assert_eq!(
            resp(b"HTTP/1.1 200 OK\r\ncontent-LENGTH: 1\r\n\r\nx")
                .unwrap()
                .1,
            b"x"
        );
    }

    #[test]
    fn bad_responses_are_errors() {
        assert!(resp(b"").unwrap_err().contains("closed"));
        assert!(resp(b"garbage\r\n\r\n").unwrap_err().contains("bad status"));
        assert!(resp(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort").is_err());
        assert!(
            resp(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n")
                .unwrap_err()
                .contains("chunk size")
        );
        let big = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert!(resp(big.as_bytes()).unwrap_err().contains("limit"));
        let big = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
            MAX_BODY + 1
        );
        assert!(resp(big.as_bytes()).unwrap_err().contains("limit"));
    }

    #[test]
    fn the_size_limit_is_exact() {
        // a body of exactly the limit is not refused for its size (it is short here, so it fails for another reason)
        let at = format!("HTTP/1.1 200 OK\r\nContent-Length: {MAX_BODY}\r\n\r\n");
        assert!(!resp(at.as_bytes()).unwrap_err().contains("limit"));
        let at = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{MAX_BODY:x}\r\n");
        assert!(!resp(at.as_bytes()).unwrap_err().contains("limit"));
        // two chunks that together pass it are refused at the second
        let two = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n{MAX_BODY:x}\r\n"
        );
        assert!(resp(two.as_bytes()).unwrap_err().contains("limit"));
        // close-delimited: the limit exactly is fine, one more byte is not
        assert_eq!(
            until_close(&mut std::io::repeat(b'x').take(MAX_BODY as u64))
                .unwrap()
                .len(),
            MAX_BODY
        );
        assert!(
            until_close(&mut std::io::repeat(b'x').take(MAX_BODY as u64 + 1))
                .unwrap_err()
                .contains("limit")
        );
    }

    #[test]
    fn until_close_ends_on_an_unclean_tls_close() {
        struct Eof;
        impl Read for Eof {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::UnexpectedEof.into())
            }
        }
        assert_eq!(until_close(&mut Eof).unwrap(), b"");
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::ConnectionReset.into())
            }
        }
        assert!(until_close(&mut Broken).is_err());
    }

    #[test]
    fn ca_file_problems_are_named() {
        assert!(roots(Some("/nonexistent/ca.pem"))
            .unwrap_err()
            .contains("/nonexistent/ca.pem"));
        let d = std::env::temp_dir().join(format!("ff-ca-{}", std::process::id()));
        std::fs::write(&d, "not a certificate").unwrap();
        assert!(roots(d.to_str()).unwrap_err().contains("no certificates"));
        std::fs::remove_file(&d).unwrap();
    }
}
