//! A fake Elasticsearch for tests: records every HTTP request and answers from a script, over plain TCP or TLS (rustls, a
//! certificate made by `pki`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Req {
    pub fn header(&self, k: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    /// The source lines of a bulk body parsed as JSON (the action lines dropped).
    pub fn docs(&self) -> Vec<serde_json::Value> {
        self.body
            .lines()
            .skip(1)
            .step_by(2)
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    pub fn actions(&self) -> Vec<serde_json::Value> {
        self.body
            .lines()
            .step_by(2)
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

pub type Script = Box<dyn FnMut(&Req) -> (u16, String) + Send>;

pub struct FakeEs {
    pub url: String,
    pub port: u16,
    pub seen: Arc<Mutex<Vec<Req>>>,
}

impl FakeEs {
    /// Answers 200 `{"errors":false,"items":[]}` to everything.
    pub fn ok() -> FakeEs {
        FakeEs::scripted(Box::new(|_| (200, r#"{"errors":false,"items":[]}"#.into())))
    }

    pub fn scripted(script: Script) -> FakeEs {
        FakeEs::serve(script, None, "http")
    }

    /// An https server whose certificate (`srv.crt`/`srv.key` in `dir`, made by `pki`) is for 127.0.0.1 only.
    pub fn tls(dir: &Path) -> FakeEs {
        use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
        let certs: Vec<_> = CertificateDer::pem_file_iter(dir.join("srv.crt"))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let key = PrivateKeyDer::from_pem_file(dir.join("srv.key")).unwrap();
        let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
        let ok = Box::new(|_: &Req| (200, r#"{"errors":false,"items":[]}"#.to_string()));
        FakeEs::serve(ok, Some(Arc::new(cfg)), "https")
    }

    fn serve(mut script: Script, tls: Option<Arc<rustls::ServerConfig>>, scheme: &str) -> FakeEs {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Req>>> = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                match &tls {
                    None => answer(c, &log, &mut script),
                    Some(cfg) => {
                        let conn = rustls::ServerConnection::new(cfg.clone()).unwrap();
                        answer(rustls::StreamOwned::new(conn, c), &log, &mut script)
                    }
                }
            }
        });
        FakeEs {
            url: format!("{scheme}://127.0.0.1:{port}"),
            port,
            seen,
        }
    }

    pub fn requests(&self) -> Vec<Req> {
        self.seen.lock().unwrap().clone()
    }

    /// Every document of every bulk request, in arrival order.
    pub fn docs(&self) -> Vec<serde_json::Value> {
        self.requests()
            .iter()
            .filter(|r| r.path == "/_bulk")
            .flat_map(Req::docs)
            .collect()
    }

    /// Wait up to `secs` for `n` documents.
    pub fn wait_docs(&self, n: usize, secs: u64) -> Vec<serde_json::Value> {
        for _ in 0..secs * 20 {
            if self.docs().len() >= n {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        self.docs()
    }
}

fn answer<S: Read + Write>(mut c: S, log: &Mutex<Vec<Req>>, script: &mut Script) {
    let Some(req) = read(&mut c) else { return };
    log.lock().unwrap().push(req.clone());
    let (code, body) = script(&req);
    let _ = write!(
        c,
        "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = c.flush();
}

fn read<S: Read>(c: &mut S) -> Option<Req> {
    let mut r = BufReader::new(c);
    let mut first = String::new();
    r.read_line(&mut first).ok()?;
    let mut p = first.split_whitespace();
    let (method, path) = (p.next()?.to_string(), p.next()?.to_string());
    let (mut headers, mut len) = (vec![], 0);
    loop {
        let mut l = String::new();
        r.read_line(&mut l).ok()?;
        let Some((k, v)) = l.trim_end().split_once(':') else {
            break;
        };
        let (k, v) = (k.to_ascii_lowercase(), v.trim().to_string());
        if k == "content-length" {
            len = v.parse().ok()?;
        }
        headers.push((k, v));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some(Req {
        method,
        path,
        headers,
        body: String::from_utf8(body).ok()?,
    })
}

fn openssl(dir: &Path, args: &[&str]) {
    let ok = Command::new("openssl")
        .current_dir(dir)
        .args(args)
        .stderr(Stdio::null())
        .status()
        .expect("openssl")
        .success();
    assert!(ok, "openssl {args:?}");
}

/// A throwaway CA (`ca.crt`) and a server certificate (`srv.crt`, `srv.key`) for the IP 127.0.0.1 only, as PEM in `dir`.
pub fn pki(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("san.ext"), "subjectAltName=IP:127.0.0.1\n").unwrap();
    let ca = [
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        "ca.key",
        "-out",
        "ca.crt",
        "-subj",
        "/CN=testca",
        "-days",
        "2",
    ];
    openssl(dir, &ca);
    let csr = [
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        "srv.key",
        "-out",
        "srv.csr",
        "-subj",
        "/CN=localhost",
    ];
    openssl(dir, &csr);
    let sign = [
        "x509",
        "-req",
        "-in",
        "srv.csr",
        "-CA",
        "ca.crt",
        "-CAkey",
        "ca.key",
        "-CAcreateserial",
        "-out",
        "srv.crt",
        "-days",
        "2",
        "-extfile",
        "san.ext",
    ];
    openssl(dir, &sign);
}
