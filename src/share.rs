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

/// A running share: maybe making a copy for the link, then uploading.
pub struct Upload {
    pub clip: PathBuf,
    /// Thousandths done of the copy being made; `DONE` once uploading.
    preparing: Arc<AtomicU64>,
    sent: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    pub cancel: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<Link, String>>>>,
}

const DONE: u64 = u64::MAX;

impl Upload {
    /// How far along the whole thing is, 0 to 1 (making the copy is the first third).
    pub fn progress(&self) -> f32 {
        let (stage, f) = self.stage();
        if stage == "Preparing" {
            f / 3.0
        } else {
            1.0 / 3.0 + f * 2.0 / 3.0
        }
    }

    /// "Preparing" or "Uploading", and how far along that is.
    pub fn stage(&self) -> (&'static str, f32) {
        match self.preparing.load(Ordering::Relaxed) {
            DONE => {
                let sent = self.sent.load(Ordering::Relaxed) as f64;
                (
                    "Uploading",
                    (sent / self.total.load(Ordering::Relaxed).max(1) as f64).min(1.0) as f32,
                )
            }
            p => ("Preparing", (p as f32 / 1000.0).min(1.0)),
        }
    }

    /// "Uploading 42%".
    pub fn label(&self) -> String {
        let (stage, f) = self.stage();
        format!("{stage} {:.0}%", f * 100.0)
    }

    /// The finished result, once.
    pub fn take(&self) -> Option<Result<Link, String>> {
        self.result.lock().unwrap().take()
    }
}

/// Copies bigger than this are made for links (or anything over 1080p or 60 fps).
const DIRECT_MAX: u64 = 60_000_000;
/// The size a link copy aims under: catbox takes 200 MB.
const COPY_TARGET: u64 = 190_000_000;

/// Starts sharing `file` (or its `range`, in seconds) on a thread. When it works, the link is
/// remembered for `clip` (the clip the file was made from, usually the file itself).
pub fn start(file: &Path, clip: &Path, setting: &str, range: Option<(f64, f64)>) -> Upload {
    let up = Upload {
        clip: clip.to_path_buf(),
        preparing: Arc::new(AtomicU64::new(0)),
        sent: Arc::default(),
        total: Arc::new(AtomicU64::new(1)),
        cancel: Arc::default(),
        result: Arc::default(),
    };
    let (file, clip) = (file.to_path_buf(), clip.to_path_buf());
    let (preparing, sent, total, cancel, result) = (
        up.preparing.clone(),
        up.sent.clone(),
        up.total.clone(),
        up.cancel.clone(),
        up.result.clone(),
    );
    let expiry = Expiry::from_setting(setting);
    std::thread::spawn(move || {
        let copy = prepare(&file, range, &preparing, &cancel);
        preparing.store(DONE, Ordering::Relaxed);
        let r = copy.and_then(|c| {
            let send = c.as_deref().unwrap_or(&file);
            total.store(
                std::fs::metadata(send).map(|m| m.len()).unwrap_or(1).max(1),
                Ordering::Relaxed,
            );
            let r = upload(send, expiry, &sent, &cancel);
            if let Some(c) = &c {
                let _ = std::fs::remove_file(c);
            }
            r
        });
        let r = r.inspect(|link| {
            let mut lib = Library::load();
            lib.set_link(&clip, Some(link.clone()));
            if let Err(e) = lib.save() {
                warning!("couldn't remember the link: {e}");
            }
        });
        if let Err(e) = &r
            && e != "Cancelled"
        {
            warning!("sharing {} failed: {e}", file.display());
        }
        *result.lock().unwrap() = Some(r);
    });
    up
}

/// Makes the copy a link gets when the file is big, high-resolution or high frame rate, or when
/// only part of it is shared. `None` when the file can go as it is.
fn prepare(
    file: &Path,
    range: Option<(f64, f64)>,
    preparing: &AtomicU64,
    cancel: &Arc<AtomicBool>,
) -> Result<Option<PathBuf>, String> {
    let video = file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4"));
    if !video {
        return Ok(None);
    }
    let ff = crate::ffmpeg::path()?;
    let info = crate::ffmpeg::probe_clip(&ff, file)?;
    let size = std::fs::metadata(file).map_err(|e| e.to_string())?.len();
    let fine = size <= DIRECT_MAX && info.height <= 1080 && info.fps <= 61.0;
    if range.is_none() && fine {
        return Ok(None);
    }
    let (start, end) = range.unwrap_or((0.0, info.duration));
    let plan = rewindr::media::web_plan(COPY_TARGET, end - start, &info)?;
    let dir = rewindr::config::config_dir().join("sharing");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = dir.join(format!("{}-{}.mp4", std::process::id(), now_secs()));
    let encoder = crate::export::best_encoder(&ff, &Mutex::new(None));
    let args = rewindr::media::share_args(file, start, end, &plan, encoder, &out);
    info!(
        "making a {}p copy to share ({plan:?})",
        plan.height.unwrap_or(info.height)
    );
    crate::export::encode_with(&ff, &args, end - start, &out, cancel, &|f| {
        preparing.store((f * 1000.0) as u64, Ordering::Relaxed)
    })?;
    Ok(Some(out))
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Why an upload didn't work.
#[derive(Debug, PartialEq)]
enum Failure {
    /// The site is down or broke (a 5xx, an error page, no connection): another may work.
    Unavailable(String),
    /// The site said no to this file.
    Refused(String),
    Cancelled,
}

/// Uploads `file` and returns its link. A link meant to last goes to catbox; if catbox is
/// down, a 3-day link from litterbox is better than none. An expiring link never becomes a
/// lasting one.
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
    let first = expiry.for_file(size, gif)?;
    let mut tries = vec![(host(first), first)];
    if first == Expiry::Forever {
        tries.push((LITTERBOX, Expiry::Hours(72)));
    }
    try_hosts(&tries, file, sent, cancel)
}

fn host(e: Expiry) -> &'static str {
    match e {
        Expiry::Forever => CATBOX,
        Expiry::Hours(_) => LITTERBOX,
    }
}

/// Tries each site in turn until one takes the file.
fn try_hosts(
    tries: &[(&str, Expiry)],
    file: &Path,
    sent: &Arc<AtomicU64>,
    cancel: &Arc<AtomicBool>,
) -> Result<Link, String> {
    let mut last = String::new();
    for &(url, e) in tries {
        sent.store(0, Ordering::Relaxed);
        let fields: Vec<(&str, String)> = match e {
            Expiry::Forever => vec![("reqtype", "fileupload".into())],
            Expiry::Hours(h) => vec![("reqtype", "fileupload".into()), ("time", format!("{h}h"))],
        };
        match post_file(url, &fields, file, sent, cancel) {
            Ok(link_url) => {
                let created = now_secs();
                info!("shared {} as {link_url}", file.display());
                return Ok(Link {
                    url: link_url,
                    created,
                    expires: match e {
                        Expiry::Forever => None,
                        Expiry::Hours(h) => Some(created + h as u64 * 3600),
                    },
                });
            }
            Err(Failure::Cancelled) => return Err("Cancelled".into()),
            Err(Failure::Refused(why)) => return Err(why),
            Err(Failure::Unavailable(why)) => {
                warning!("{url}: {why}");
                last = why;
            }
        }
    }
    Err(format!("{last} Try again in a few minutes."))
}

/// What a site's answer means, from its status and text.
fn judge(site: &str, status: u16, text: &str) -> Result<String, Failure> {
    let text = text.trim();
    if (200..300).contains(&status)
        && text.starts_with("https://")
        && !text.contains(char::is_whitespace)
    {
        return Ok(text.to_string());
    }
    // An error page: say what it was called, not its HTML.
    let html = text.starts_with('<') || text.to_ascii_lowercase().contains("<html");
    let what = if html {
        let lower = text.to_ascii_lowercase();
        lower
            .find("<title>")
            .and_then(|i| {
                let rest = &text[i + 7..];
                rest.find('<')
                    .map(|j| rest[..j].trim().replace(" | ", " ").to_string())
            })
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("error {status}"))
    } else if text.is_empty() {
        format!("error {status}")
    } else {
        text.chars().take(160).collect()
    };
    if status >= 500 || html || status == 0 {
        Err(Failure::Unavailable(format!(
            "{site} isn't working right now ({what})."
        )))
    } else {
        Err(Failure::Refused(format!("{site} wouldn't take it: {what}")))
    }
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
) -> Result<String, Failure> {
    let site = url.split('/').nth(2).unwrap_or(url).to_string();
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
    let f = std::fs::File::open(file)
        .map_err(|e| Failure::Refused(format!("{}: {e}", file.display())))?;
    let size = f
        .metadata()
        .map_err(|e| Failure::Refused(e.to_string()))?
        .len();
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
        return Err(Failure::Cancelled);
    }
    let mut response =
        response.map_err(|e| Failure::Unavailable(format!("Couldn't reach {site} ({e}).")))?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    judge(&site, status, &text)
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
        assert!(
            matches!(&e, Failure::Refused(m) if m.contains("File type not allowed")),
            "{e:?}"
        );
        let _ = server.join();
    }

    #[test]
    fn error_pages_read_as_sentences() {
        let page = "<!doctype html><html lang=\"en\"><meta charset=\"UTF-8\"><title>500 | Internal Server Error</title><style>x{}</style>";
        assert_eq!(
            judge("litterbox.catbox.moe", 500, page),
            Err(Failure::Unavailable(
                "litterbox.catbox.moe isn't working right now (500 Internal Server Error).".into()
            ))
        );
        assert!(matches!(
            judge("catbox.moe", 502, ""),
            Err(Failure::Unavailable(_))
        ));
        assert!(matches!(
            judge("catbox.moe", 200, "Too big"),
            Err(Failure::Refused(_))
        ));
        assert_eq!(
            judge("catbox.moe", 200, "https://files.catbox.moe/a.mp4\n"),
            Ok("https://files.catbox.moe/a.mp4".into())
        );
    }

    #[test]
    fn a_lasting_link_falls_back_to_a_three_day_one() {
        // Both sites "down": the message comes from the last one tried, and says to retry.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.mp4");
        std::fs::write(&file, b"x").unwrap();
        let tries = [
            ("http://127.0.0.1:9/a", Expiry::Forever),
            ("http://127.0.0.1:9/b", Expiry::Hours(72)),
        ];
        let e = try_hosts(&tries, &file, &Arc::default(), &Arc::default()).unwrap_err();
        assert!(e.contains("Try again"), "{e}");
    }
}
