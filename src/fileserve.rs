//! Built-in static file server: shares a directory (or single file) as a
//! browsable HTTPS site behind the edge.
//!
//! Runs on an ephemeral loopback port; the rest of the pipeline treats it
//! like any other port target. Paths are canonicalized and must stay under
//! the canonicalized root — symlinks pointing outside are not followed.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bytes::Bytes;
use futures_util::TryStreamExt;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::Frame;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;

/// Characters to escape in listing links, beyond controls.
const LINK_ESCAPE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'#')
    .add(b'?')
    .add(b'%');

type FileBody = BoxBody<Bytes, std::io::Error>;

/// Start serving `root` (a directory or a single file) on an ephemeral
/// loopback port. Returns the port; the server runs until the process exits.
pub async fn serve(root: PathBuf) -> Result<u16> {
    let root = tokio::fs::canonicalize(&root)
        .await
        .with_context(|| format!("cannot access {}", root.display()))?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let root = root.clone();
            tokio::spawn(async move {
                let service = service_fn(move |req| {
                    let root = root.clone();
                    async move { Ok::<_, std::convert::Infallible>(handle(&root, req).await) }
                });
                if let Err(e) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(tcp), service)
                    .await
                {
                    tracing::debug!("fileserve connection ended: {e}");
                }
            });
        }
    });
    Ok(port)
}

async fn handle<B>(root: &Path, req: Request<B>) -> Response<FileBody> {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return text_response(StatusCode::METHOD_NOT_ALLOWED, "GET and HEAD only");
    }
    match resolve(root, req.uri().path()).await {
        Some(path) if path.is_dir() => match listing(root, &path).await {
            Ok(html) => html_response(StatusCode::OK, html),
            Err(e) => {
                tracing::warn!("listing {} failed: {e}", path.display());
                text_response(StatusCode::INTERNAL_SERVER_ERROR, "listing failed")
            }
        },
        Some(path) => match tokio::fs::File::open(&path).await {
            Ok(file) => {
                let mime = mime_guess::from_path(&path).first_or_octet_stream();
                let len = file.metadata().await.map(|m| m.len()).ok();
                let stream = ReaderStream::new(file).map_ok(Frame::data);
                let mut resp = Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", mime.as_ref());
                if let Some(len) = len {
                    resp = resp.header("content-length", len);
                }
                resp.body(BodyExt::boxed(StreamBody::new(stream)))
                    .expect("static response")
            }
            Err(e) => {
                tracing::debug!("open {} failed: {e}", path.display());
                text_response(StatusCode::NOT_FOUND, "not found")
            }
        },
        None => text_response(StatusCode::NOT_FOUND, "not found"),
    }
}

/// Map a request path to a filesystem path that is provably under `root`
/// (which is already canonical). Any escape attempt resolves to None.
async fn resolve(root: &Path, uri_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode_str(uri_path).decode_utf8().ok()?;
    // A single-file root is served at every path ("/", "/whatever").
    if root.is_file() {
        return Some(root.to_path_buf());
    }
    let mut path = root.to_path_buf();
    for part in decoded.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            part => path.push(part),
        }
    }
    let canonical = tokio::fs::canonicalize(&path).await.ok()?;
    canonical.starts_with(root).then_some(canonical)
}

async fn listing(root: &Path, dir: &Path) -> Result<String> {
    let rel = dir.strip_prefix(root).unwrap_or(dir);
    let title = format!("/{}", rel.display());
    let mut entries = Vec::new();
    let mut read = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = read.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
        let size = if is_dir {
            String::new()
        } else {
            entry
                .metadata()
                .await
                .map(|m| human_size(m.len()))
                .unwrap_or_default()
        };
        entries.push((is_dir, name, size));
    }
    entries.sort_by_key(|e| (!e.0, e.1.to_lowercase()));

    let mut rows = String::new();
    if rel.components().next().is_some() {
        rows.push_str("<tr><td><a href=\"../\">../</a></td><td></td></tr>");
    }
    for (is_dir, name, size) in entries {
        let slash = if is_dir { "/" } else { "" };
        let href = format!("{}{slash}", utf8_percent_encode(&name, LINK_ESCAPE));
        let shown = html_escape(&name);
        rows.push_str(&format!(
            "<tr><td><a href=\"{href}\">{shown}{slash}</a></td><td>{size}</td></tr>"
        ));
    }
    Ok(format!(
        "<!doctype html><html><head><title>{title} — lclhst</title>\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <style>body{{font-family:system-ui;max-width:42rem;margin:2rem auto;padding:0 1rem;color:#333}}\
         table{{width:100%;border-collapse:collapse}}td{{padding:.35rem .5rem;border-bottom:1px solid #eee}}\
         td:last-child{{text-align:right;color:#888;white-space:nowrap}}a{{text-decoration:none}}</style>\
         </head><body><h2>{title}</h2><table>{rows}</table></body></html>"
    ))
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn html_response(status: StatusCode, html: String) -> Response<FileBody> {
    Response::builder()
        .status(status)
        .header("content-type", "text/html; charset=utf-8")
        .body(full(html))
        .expect("static response")
}

fn text_response(status: StatusCode, msg: &str) -> Response<FileBody> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(full(msg.to_string()))
        .expect("static response")
}

fn full(s: String) -> FileBody {
    Full::new(Bytes::from(s))
        .map_err(|never| match never {})
        .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (tempdir::Root, u16) {
        let root = tempdir::make().await;
        let port = serve(root.path.clone()).await.unwrap();
        (root, port)
    }

    /// Tiny tempdir helper without a dev-dependency.
    mod tempdir {
        use std::path::PathBuf;

        pub struct Root {
            pub path: PathBuf,
        }
        impl Drop for Root {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.path).ok();
            }
        }

        pub async fn make() -> Root {
            let path = std::env::temp_dir().join(format!(
                "lclhst-test-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            tokio::fs::create_dir_all(path.join("sub")).await.unwrap();
            tokio::fs::write(path.join("hello.txt"), b"hi there")
                .await
                .unwrap();
            tokio::fs::write(path.join("sub/page.html"), b"<h1>deep</h1>")
                .await
                .unwrap();
            tokio::fs::write(path.join("with space.txt"), b"spaced")
                .await
                .unwrap();
            Root { path }
        }
    }

    async fn get(port: u16, path: &str) -> (u16, String, String) {
        let resp = reqwest::get(format!("http://127.0.0.1:{port}{path}"))
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let ctype = resp
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        (status, ctype, resp.text().await.unwrap())
    }

    #[tokio::test]
    async fn serves_files_with_mime_types() {
        let (_root, port) = fixture().await;
        let (status, ctype, body) = get(port, "/hello.txt").await;
        assert_eq!(status, 200);
        assert!(ctype.starts_with("text/plain"), "{ctype}");
        assert_eq!(body, "hi there");

        let (status, ctype, body) = get(port, "/sub/page.html").await;
        assert_eq!(status, 200);
        assert!(ctype.starts_with("text/html"), "{ctype}");
        assert_eq!(body, "<h1>deep</h1>");
    }

    #[tokio::test]
    async fn lists_directories_with_encoded_links() {
        let (_root, port) = fixture().await;
        let (status, ctype, body) = get(port, "/").await;
        assert_eq!(status, 200);
        assert!(ctype.starts_with("text/html"));
        assert!(body.contains("hello.txt"));
        assert!(body.contains("sub/"), "dirs listed with trailing slash");
        assert!(
            body.contains("with%20space.txt"),
            "links percent-encoded: {body}"
        );

        let (status, _, body) = get(port, "/sub/").await;
        assert_eq!(status, 200);
        assert!(body.contains("page.html"));
    }

    #[tokio::test]
    async fn decodes_percent_encoded_paths() {
        let (_root, port) = fixture().await;
        let (status, _, body) = get(port, "/with%20space.txt").await;
        assert_eq!(status, 200);
        assert_eq!(body, "spaced");
    }

    #[tokio::test]
    async fn refuses_to_escape_the_root() {
        let (_root, port) = fixture().await;
        for sneaky in [
            "/../etc/passwd",
            "/%2e%2e/etc/passwd",
            "/sub/%2e%2e/%2e%2e/etc/passwd",
        ] {
            let (status, _, _) = get(port, sneaky).await;
            assert_eq!(status, 404, "{sneaky} must not escape the root");
        }
    }

    #[tokio::test]
    async fn missing_file_is_404_and_post_is_405() {
        let (_root, port) = fixture().await;
        let (status, _, _) = get(port, "/nope.txt").await;
        assert_eq!(status, 404);

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/hello.txt"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 405);
    }

    #[tokio::test]
    async fn serves_a_single_file_at_root() {
        let root = tempdir::make().await;
        let port = serve(root.path.join("hello.txt")).await.unwrap();
        let (status, _, body) = get(port, "/").await;
        assert_eq!(status, 200);
        assert_eq!(body, "hi there");
    }
}
