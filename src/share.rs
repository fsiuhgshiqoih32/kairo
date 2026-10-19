//! Share links: uploads a clip to catbox.moe (kept while people keep watching it) or to
//! litterbox.catbox.moe (gone after 1 to 72 hours) and returns the link. Anyone with the link
//! can watch the clip; Discord plays the video right in the chat.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use rewindr::library::{Library, Link};

use crate::log::{info, warning};

const CATBOX: &str = "https://catbox.moe/user/api.php";
const LITTERBOX: &str = "https://litterbox.catbox.moe/resources/internals/api.php";
/// Catbox's upload limits.
const CATBOX_MAX: u64 = 200_000_000;
const LITTERBOX_MAX: u64 = 1_000_000_000;
/// Catbox refuses GIFs over 20 MB.
const CATBOX_GIF_MAX: u64 = 20_000_000;

/// Where an upload goes and how long it lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expiry {
    Forever,
    Hours(u32),
}

impl Expiry {
    /// From the `share_expiry` setting: "forever", "72h", "24h", "12h", "1h".
    pub fn from_setting(s: &str) -> Expiry {
        match s.trim_end_matches('h').parse::<u32>() {
            Ok(h) if matches!(h, 1 | 12 | 24 | 72) => Expiry::Hours(h),
            _ => Expiry::Forever,
        }
    }

    /// What it will be for a file this big: catbox takes at most 200 MB (20 MB for GIFs), so
    /// bigger files get a 3-day link.
    pub fn for_file(self, size: u64, gif: bool) -> Result<Expiry, String> {
        if size > LITTERBOX_MAX {
            return Err("Clips over 1 GB can't be shared as a link. Trim it shorter first.".into());
        }
        let catbox_max = if gif { CATBOX_GIF_MAX } else { CATBOX_MAX };
        Ok(match self {
            Expiry::Forever if size > catbox_max => Expiry::Hours(72),
            e => e,
        })
    }
}

/// A running upload.
pub struct Upload {
    pub clip: PathBuf,
    pub sent: Arc<AtomicU64>,
    pub total: u64,
    pub cancel: Arc<AtomicBool>,
    pub result: Arc<Mutex<Option<Result<Link, String>>>>,
}

impl Upload {
    pub fn progress(&self) -> f32 {
        (self.sent.load(Ordering::Relaxed) as f64 / self.total.max(1) as f64).min(1.0) as f32
    }

    /// The finished result, once.
    pub fn take(&self) -> Option<Result<Link, String>> {
        self.result.lock().unwrap().take()
    }
}

/// Starts uploading `file` on a thread. When it works, the link is remembered for `clip` (the
/// clip the file was made from, often the file itself).
pub fn start(file: &Path, clip: &Path, setting: &str) -> Upload {
    let total = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    let up = Upload {
        clip: clip.to_path_buf(),
        sent: Arc::default(),
        total,
        cancel: Arc::default(),
        result: Arc::default(),
    };
    let (file, clip) = (file.to_path_buf(), clip.to_path_buf());
    let (sent, cancel, result) = (up.sent.clone(), up.cancel.clone(), up.result.clone());
    let expiry = Expiry::from_setting(setting);
    std::thread::spawn(move || {
        let r = upload(&file, expiry, &sent, &cancel).inspect(|link| {
            let mut lib = Library::load();
            lib.set_link(&clip, Some(link.clone()));
            if let Err(e) = lib.save() {
                warning!("couldn't remember the link: {e}");
            }
        });
        if let Err(e) = &r
            && e != "Cancelled"
        {
            warning!("upload failed: {e}");
        }
        *result.lock().unwrap() = Some(r);
    });
    up
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Uploads `file` and returns its link.
pub fn upload(
    file: &Path,
    expiry: Expiry,
    sent: &Arc<AtomicU64>,
    cancel: &Arc<AtomicBool>,
) -> Result<Link, String> {
    let size = std::fs::metadata(file)
        .map_err(|e| format!("{}: {e}", file.display()))?
        .len();
    let gif = file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"));
    let expiry = expiry.for_file(size, gif)?;
    let (url, fields): (&str, Vec<(&str, String)>) = match expiry {
        Expiry::Forever => (CATBOX, vec![("reqtype", "fileupload".into())]),
        Expiry::Hours(h) => (
            LITTERBOX,
            vec![("reqtype", "fileupload".into()), ("time", format!("{h}h"))],
        ),
    };
    let link_url = post_file(url, &fields, file, sent, cancel)?;
    let created = now_secs();
    info!("shared {} as {link_url}", file.display());
    Ok(Link {
        url: link_url,
        created,
        expires: match expiry {
            Expiry::Forever => None,
            Expiry::Hours(h) => Some(created + h as u64 * 3600),
        },
    })
}

fn content_type(file: &Path) -> &'static str {
    match file
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("mp4") => "video/mp4",
        Some("gif") => "image/gif",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

/// The parts of a multipart/form-data body around the file's bytes.
fn multipart(
    boundary: &str,
    fields: &[(&str, String)],
    file_name: &str,
    mime: &str,
) -> (Vec<u8>, Vec<u8>) {
    let mut head = String::new();
    for (k, v) in fields {
        head.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"
        ));
    }
    let safe: String = file_name
        .chars()
        .map(|c| if c == '"' || c.is_control() { '_' } else { c })
        .collect();
    head.push_str(&format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"fileToUpload\"; filename=\"{safe}\"\r\nContent-Type: {mime}\r\n\r\n"
    ));
    (
        head.into_bytes(),
        format!("\r\n--{boundary}--\r\n").into_bytes(),
    )
}

/// Reads the body and counts what's been sent; stops when cancelled.
struct Counting<R> {
    inner: R,
    sent: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("Cancelled"));
        }
        let n = self.inner.read(buf)?;
        self.sent.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// POSTs `file` with `fields` as multipart/form-data and returns the response text (the link).
fn post_file(
    url: &str,
    fields: &[(&str, String)],
    file: &Path,
    sent: &Arc<AtomicU64>,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "clip.mp4".into());
    let boundary = format!(
        "rewindr{:x}{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let (head, tail) = multipart(&boundary, fields, &name, content_type(file));
    let f = std::fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    let length = head.len() as u64 + size + tail.len() as u64;
    let body = Counting {
        inner: Cursor::new(head).chain(f).chain(Cursor::new(tail)),
        sent: sent.clone(),
        cancel: cancel.clone(),
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(180)))
        .user_agent(concat!(
            "Rewindr/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/fsiuhgshiqoih32/rewindr)"
        ))
        .http_status_as_error(false)
        .build()
        .into();
    let response = agent
        .post(url)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header("Content-Length", length.to_string())
        .send(ureq::SendBody::from_owned_reader(body));
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".into());
    }
    let mut response = response.map_err(|e| format!("Couldn't upload: {e}"))?;
    let status = response.status();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let text = text.trim();
    if status.is_success() && text.starts_with("https://") && !text.contains(char::is_whitespace) {
        Ok(text.to_string())
    } else {
        let why = if text.is_empty() {
            status.to_string()
        } else {
            text.chars().take(200).collect()
        };
        Err(format!("The upload site said: {why}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn expiry_from_settings_and_size() {
        assert_eq!(Expiry::from_setting("forever"), Expiry::Forever);
        assert_eq!(Expiry::from_setting("72h"), Expiry::Hours(72));
        assert_eq!(Expiry::from_setting("5h"), Expiry::Forever);
        assert_eq!(
            Expiry::Forever.for_file(150_000_000, false),
            Ok(Expiry::Forever)
        );
        assert_eq!(
            Expiry::Forever.for_file(250_000_000, false),
            Ok(Expiry::Hours(72))
        );
        assert_eq!(
            Expiry::Forever.for_file(25_000_000, true),
            Ok(Expiry::Hours(72))
        );
        assert_eq!(
            Expiry::Hours(1).for_file(250_000_000, false),
            Ok(Expiry::Hours(1))
        );
        assert!(Expiry::Forever.for_file(2_000_000_000, false).is_err());
    }

    /// A one-request HTTP server that records the request and answers with `reply`.
    fn serve_once(reply: &'static str) -> (String, std::thread::JoinHandle<(String, Vec<u8>)>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api.php", listener.local_addr().unwrap());
        let t = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut headers = String::new();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                headers.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0u8; length];
            r.read_exact(&mut body).unwrap();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            s.write_all(resp.as_bytes()).unwrap();
            (headers, body)
        });
        (url, t)
    }

    #[test]
    fn uploads_a_file_as_multipart_form_data() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Clip one.mp4");
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&file, &data).unwrap();
        let (url, server) = serve_once("https://files.catbox.moe/abc123.mp4\n");
        let sent = Arc::new(AtomicU64::new(0));
        let fields = vec![
            ("reqtype", "fileupload".to_string()),
            ("time", "72h".to_string()),
        ];
        let link = post_file(&url, &fields, &file, &sent, &Arc::default()).unwrap();
        assert_eq!(link, "https://files.catbox.moe/abc123.mp4");
        let (headers, body) = server.join().unwrap();
        let lower = headers.to_ascii_lowercase();
        assert!(
            lower.contains("content-type: multipart/form-data; boundary=rewindr"),
            "{headers}"
        );
        assert!(
            !lower.contains("transfer-encoding"),
            "sent with a length, not chunked"
        );
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("name=\"reqtype\"\r\n\r\nfileupload\r\n"));
        assert!(text.contains("name=\"time\"\r\n\r\n72h\r\n"));
        let part = "filename=\"Clip one.mp4\"\r\nContent-Type: video/mp4\r\n\r\n";
        assert!(text.contains(part));
        // The file's bytes arrive unchanged right after the part header.
        let start = body
            .windows(part.len())
            .position(|w| w == part.as_bytes())
            .unwrap()
            + part.len();
        assert_eq!(&body[start..start + data.len()], &data[..]);
        assert_eq!(&body[start + data.len()..start + data.len() + 4], b"\r\n--");
        assert!(body.ends_with(b"--\r\n"));
        assert_eq!(sent.load(Ordering::Relaxed), body.len() as u64);
    }

    #[test]
    fn quotes_in_file_names_dont_break_the_form() {
        let (head, tail) = multipart("b", &[], "say \"hi\".mp4", "video/mp4");
        let head = String::from_utf8(head).unwrap();
        assert!(head.contains("filename=\"say _hi_.mp4\""), "{head}");
        assert_eq!(tail, b"\r\n--b--\r\n");
    }

    #[test]
    fn an_error_page_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.mp4");
        std::fs::write(&file, b"x").unwrap();
        let (url, server) = serve_once("File type not allowed.");
        let e = post_file(&url, &[], &file, &Arc::default(), &Arc::default()).unwrap_err();
        assert!(e.contains("File type not allowed"), "{e}");
        let _ = server.join();
    }
}
