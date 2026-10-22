//! Friends: a Kairo account (signed in with Google), friends found by username, and clips and
//! messages sent between them.
//!
//! Kairo's Supabase project keeps the accounts, friendships and messages; `supabase/setup.sql`
//! makes its tables and the rules on who may see and change what. Clips aren't stored there: a
//! clip goes as its share link (catbox.moe), made the usual way.
//!
//! Signing in opens the browser on Google (OAuth with PKCE) and listens on
//! `127.0.0.1:47615` for the code Supabase sends back. The long-lived refresh token goes in the
//! OS password store, shared by the window and the background process; each keeps its own
//! short-lived access token. A refresh token works once, so refreshing happens under a lock and
//! reads the newest one first.
//!
//! The background process asks what's new every half minute, keeps it in `friends.json` for the
//! window, and shows a pop-up for each new message and friend request.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::log::{info, warning};
use crate::recorder::Shared;
use crate::single_instance;

/// Kairo's Supabase project: its address and publishable key. Both are public by design (every
/// copy of Kairo carries them); the database's rules decide what each signed-in person may do.
/// `KAIRO_SUPABASE_URL` and `KAIRO_SUPABASE_KEY` point a copy at another project.
const PROJECT_URL: &str = "";
const PUBLISHABLE_KEY: &str = "";

/// Where Supabase sends the browser back to after Google, on this computer. It has to be in the
/// project's list of redirect URLs.
pub const CALLBACK: &str = "http://127.0.0.1:47615/callback";
const CALLBACK_PORT: u16 = 47615;

/// The password-store entry with the account (refresh token, user id, email).
const SECRET: &str = "account";
/// Held while refreshing the session, across processes.
const LOCK: &str = "account.lock";

/// Messages kept on this computer, newest last.
const KEEP: usize = 2000;
/// How often the background process asks what's new.
const EVERY: Duration = Duration::from_secs(30);

pub const SIGNED_OUT: &str = "You're signed out. Sign in with Google to see your friends.";

struct Server {
    url: String,
    key: String,
}

fn server() -> Option<Server> {
    let url = std::env::var("KAIRO_SUPABASE_URL").unwrap_or_else(|_| PROJECT_URL.into());
    let key = std::env::var("KAIRO_SUPABASE_KEY").unwrap_or_else(|_| PUBLISHABLE_KEY.into());
    (!url.trim().is_empty() && !key.trim().is_empty()).then(|| Server {
        url: url.trim().trim_end_matches('/').to_string(),
        key: key.trim().to_string(),
    })
}

/// Whether this copy of Kairo has a friends server to talk to.
pub fn available() -> bool {
    server().is_some()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

// --- The account ---------------------------------------------------------------------------

/// What the password store keeps: enough to get a new access token, and who it's for.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Stored {
    refresh_token: String,
    user_id: String,
    #[serde(default)]
    email: Option<String>,
}

fn stored() -> Option<Stored> {
    crate::secret::get(SECRET).and_then(|s| serde_json::from_str(&s).ok())
}

/// This process's access token: whose it is, the token, and when it runs out (Unix seconds).
/// Another process may sign out or in as someone else meanwhile, so it counts only for the
/// account in the password store.
static ACCESS: Mutex<Option<(String, String, i64)>> = Mutex::new(None);

pub fn signed_in() -> bool {
    stored().is_some()
}

/// The email the account signed in with.
pub fn email() -> Option<String> {
    stored().and_then(|s| s.email)
}

/// The signed-in person, with an access token good for at least two more minutes.
struct Account {
    id: String,
    token: String,
}

fn account(server: &Server) -> Result<Account, String> {
    let s = stored().ok_or(SIGNED_OUT)?;
    if let Some((user, token, until)) = ACCESS.lock().unwrap().clone()
        && user == s.user_id
        && until - now() > 120
    {
        return Ok(Account {
            id: s.user_id,
            token,
        });
    }
    let _lock = single_instance::acquire_within(LOCK, Duration::from_secs(30))?
        .ok_or("Kairo's account is busy. Try again in a moment.")?;
    // Another Kairo may have refreshed meanwhile: its token is the one that works now.
    let s = stored().ok_or(SIGNED_OUT)?;
    let (status, v) = call(
        server,
        "POST",
        "/auth/v1/token?grant_type=refresh_token",
        None,
        Some(json!({ "refresh_token": s.refresh_token })),
        &[],
    )?;
    match status {
        200 => keep_session(&v).map(|(id, token)| Account { id, token }),
        400 | 401 | 403 => {
            crate::secret::remove(SECRET);
            *ACCESS.lock().unwrap() = None;
            Err("Your sign-in ran out. Sign in with Google again.".into())
        }
        _ => Err(problem(status, &v)),
    }
}

/// Keeps a session from Supabase Auth (a sign-in or a refresh): the refresh token in the
/// password store, the access token in this process.
fn keep_session(v: &Value) -> Result<(String, String), String> {
    let field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let token = field("access_token").ok_or("The sign-in came back without a token.")?;
    let refresh = field("refresh_token").ok_or("The sign-in came back without a token.")?;
    let user = v
        .get("user")
        .ok_or("The sign-in came back without an account.")?;
    let id = user
        .get("id")
        .and_then(Value::as_str)
        .ok_or("The sign-in came back without an account.")?
        .to_string();
    let until = v
        .get("expires_at")
        .and_then(Value::as_i64)
        .or_else(|| {
            v.get("expires_in")
                .and_then(Value::as_i64)
                .map(|s| now() + s)
        })
        .unwrap_or_else(|| now() + 3000);
    let s = Stored {
        refresh_token: refresh,
        user_id: id.clone(),
        email: user
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    crate::secret::set(
        SECRET,
        &serde_json::to_string(&s).map_err(|e| e.to_string())?,
    )?;
    *ACCESS.lock().unwrap() = Some((id.clone(), token.clone(), until));
    Ok((id, token))
}

/// Forgets the account on this computer (and ends the session on the server, if it can).
pub fn sign_out() {
    if let Some(server) = server()
        && let Some(Some((_, token, _))) = ACCESS.lock().ok().map(|a| a.clone())
    {
        let _ = call(&server, "POST", "/auth/v1/logout", Some(&token), None, &[]);
    }
    crate::secret::remove(SECRET);
    *ACCESS.lock().unwrap() = None;
    let _ = std::fs::remove_file(inbox_path());
}

/// Signs in with Google: opens the browser, waits up to five minutes for Supabase to send it back
/// here with a code, and trades the code for a session. `cancel` stops the wait.
pub fn sign_in(cancel: &AtomicBool) -> Result<(), String> {
    let server = server().ok_or("Friends isn't set up in this copy of Kairo.")?;
    let verifier = base64url(&random::<48>());
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    let listener = TcpListener::bind(("127.0.0.1", CALLBACK_PORT)).map_err(|e| {
        format!(
            "Kairo couldn't wait for the sign-in ({e}). Another program may be using port \
             {CALLBACK_PORT}, or another sign-in is still open."
        )
    })?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let url = format!(
        "{}/auth/v1/authorize?provider=google&redirect_to={}&code_challenge={challenge}\
         &code_challenge_method=s256",
        server.url,
        percent(CALLBACK)
    );
    opener::open_browser(&url).map_err(|e| format!("Couldn't open the browser: {e}"))?;
    let code = wait_for_code(&listener, cancel, Instant::now() + Duration::from_secs(300))?;
    let (status, v) = call(
        &server,
        "POST",
        "/auth/v1/token?grant_type=pkce",
        None,
        Some(json!({ "auth_code": code, "code_verifier": verifier })),
        &[],
    )?;
    if status != 200 {
        return Err(problem(status, &v));
    }
    keep_session(&v)?;
    info!("signed in to friends");
    Ok(())
}

/// Waits on `listener` (non-blocking) for the browser to come back with the sign-in's code.
fn wait_for_code(
    listener: &TcpListener,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<String, String> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }
        if Instant::now() > deadline {
            return Err("The sign-in took too long. Try again.".into());
        }
        match listener.accept() {
            Ok((stream, _)) => match answer_browser(stream) {
                Some(result) => return result,
                // Something else the browser asked for (its icon): keep waiting.
                None => {}
            },
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Reads the browser's request: `Some(Ok(code))` for the sign-in coming back, `Some(Err)` when it
/// came back with an error, `None` for anything else. Answers with a page to close.
fn answer_browser(mut stream: TcpStream) -> Option<Result<String, String>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).ok()?;
    let target = line.split_whitespace().nth(1).unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/callback" {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return None;
    }
    let param = |k: &str| {
        query
            .split('&')
            .filter_map(|p| p.split_once('='))
            .find(|(n, _)| *n == k)
            .map(|(_, v)| unpercent(v))
    };
    let result = match (param("code"), param("error_description").or(param("error"))) {
        (Some(code), _) => Ok(code),
        (None, Some(e)) => Err(format!("Google sign-in didn't work: {e}")),
        (None, None) => Err("Google sign-in didn't finish. Try again.".into()),
    };
    let (title, body) = match &result {
        Ok(_) => (
            "You're signed in",
            "Kairo has your account. You can close this tab.".to_string(),
        ),
        Err(e) => ("Sign-in didn't work", html_escape(e)),
    };
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>Kairo</title>\
         <body style=\"font:16px system-ui,sans-serif;background:#111;color:#eee;display:grid;\
         place-items:center;height:90vh\"><div style=\"text-align:center\"><h1>{title}</h1>\
         <p>{body}</p></div>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{page}",
        page.len()
    );
    Some(result)
}

// --- Talking to Supabase -------------------------------------------------------------------

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(concat!(
            "Kairo/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/fsiuhgshiqoih32/kairo)"
        ))
        .http_status_as_error(false)
        .build()
        .into()
}

/// One request to the project: the status and the JSON that came back (`Null` for none).
fn call(
    server: &Server,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> Result<(u16, Value), String> {
    let url = format!("{}{path}", server.url);
    let agent = agent();
    let bearer = token.map(|t| format!("Bearer {t}"));
    macro_rules! dress {
        ($req:expr) => {{
            let mut r = $req.header("apikey", &server.key);
            if let Some(b) = &bearer {
                r = r.header("Authorization", b);
            }
            for (k, v) in headers {
                r = r.header(*k, *v);
            }
            r
        }};
    }
    let sent = match (method, body) {
        ("GET", _) => dress!(agent.get(&url)).call(),
        ("DELETE", _) => dress!(agent.delete(&url)).call(),
        ("PATCH", Some(b)) => dress!(agent.patch(&url)).send_json(b),
        (_, Some(b)) => dress!(agent.post(&url)).send_json(b),
        (_, None) => dress!(agent.post(&url)).send_empty(),
    };
    let mut response = sent.map_err(|e| format!("Couldn't reach Kairo's friends server ({e})."))?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    let v = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    Ok((status, v))
}

/// What went wrong, in words: the server's own message where it gave one.
fn problem(status: u16, v: &Value) -> String {
    let said = ["message", "msg", "error_description", "error"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .map(str::to_string)
        .or_else(|| v.as_str().map(|s| s.chars().take(160).collect()));
    match (status, said) {
        (429, _) => "Too many tries at once. Wait a minute and try again.".into(),
        (s, _) if s >= 500 => {
            format!("Kairo's friends server isn't working right now (error {s}).")
        }
        (_, Some(m)) => m,
        (s, None) => format!("Kairo's friends server said no (error {s})."),
    }
}

/// A request made as the signed-in person; `Ok` only for a 2xx answer.
fn ask(
    server: &Server,
    a: &Account,
    method: &str,
    path: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> Result<Value, String> {
    let (status, v) = call(server, method, path, Some(&a.token), body, headers)?;
    if (200..300).contains(&status) {
        Ok(v)
    } else {
        Err(problem(status, &v))
    }
}

fn rows<T: for<'de> Deserialize<'de>>(v: Value) -> Result<Vec<T>, String> {
    serde_json::from_value(v).map_err(|e| format!("Kairo's friends server answered oddly ({e})."))
}

// --- People, friends and messages ----------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Profile {
    pub id: String,
    pub username: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    Friends,
    /// They asked; you haven't accepted yet.
    AskedYou,
    /// You asked; they haven't accepted yet.
    YouAsked,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Friend {
    pub id: String,
    pub username: String,
    pub standing: Standing,
}

/// Some text, a clip, or both, from one person to another.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Message {
    pub id: i64,
    pub sender: String,
    pub recipient: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub clip_url: Option<String>,
    #[serde(default)]
    pub clip_title: Option<String>,
    #[serde(default)]
    pub clip_seconds: Option<f64>,
    pub created_at: String,
    #[serde(default)]
    pub read_at: Option<String>,
}

impl Message {
    /// One line for a list or a pop-up.
    pub fn preview(&self) -> String {
        match (&self.clip_title, self.body.trim()) {
            (Some(t), "") => format!("Sent a clip: {t}"),
            (None, "") if self.clip_url.is_some() => "Sent a clip".into(),
            (_, b) => b.lines().next().unwrap_or("").chars().take(120).collect(),
        }
    }
}

/// A clip to send: its share link, name and length.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipLink {
    pub url: String,
    pub title: String,
    pub seconds: f64,
}

/// Your username, friends and messages as last fetched: kept in `friends.json`, so the window
/// shows them at once and works offline.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Inbox {
    /// Your profile; `None` until you pick a username.
    pub me: Option<Profile>,
    /// Who the account is, to tell when someone else signed in.
    #[serde(default)]
    pub account: String,
    pub friends: Vec<Friend>,
    /// Sent and received, oldest first.
    pub messages: Vec<Message>,
}

impl Inbox {
    /// Messages between you and `friend`, oldest first.
    pub fn chat<'a>(&'a self, friend: &'a str) -> impl Iterator<Item = &'a Message> + 'a {
        self.messages
            .iter()
            .filter(move |m| m.sender == friend || m.recipient == friend)
    }

    /// Messages from `friend` you haven't opened.
    pub fn unread(&self, friend: &str) -> usize {
        let me = self.me.as_ref().map_or("", |p| p.id.as_str());
        self.messages
            .iter()
            .filter(|m| m.sender == friend && m.recipient == me && m.read_at.is_none())
            .count()
    }

    pub fn name_of(&self, id: &str) -> String {
        self.friends
            .iter()
            .find(|f| f.id == id)
            .map_or_else(|| "Someone".into(), |f| f.username.clone())
    }

    pub fn friends_only(&self) -> impl Iterator<Item = &Friend> {
        self.friends
            .iter()
            .filter(|f| f.standing == Standing::Friends)
    }

    fn add(&mut self, new: Vec<Message>) {
        for m in new {
            match self.messages.iter_mut().find(|o| o.id == m.id) {
                Some(o) => *o = m,
                None => self.messages.push(m),
            }
        }
        self.messages.sort_by_key(|m| m.id);
        let extra = self.messages.len().saturating_sub(KEEP);
        self.messages.drain(..extra);
    }
}

fn inbox_path() -> PathBuf {
    kairo::config::config_dir().join("friends.json")
}

/// What `friends.json` holds (empty if nothing yet).
pub fn inbox() -> Inbox {
    std::fs::read_to_string(inbox_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_inbox(inbox: &Inbox) {
    let path = inbox_path();
    let tmp = path.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_string(inbox)
        && std::fs::write(&tmp, text).is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Fetches what's new (your profile, friends, and messages after the newest one held), keeps it
/// in `friends.json` and returns it.
pub fn sync() -> Result<Inbox, String> {
    let server = server().ok_or("Friends isn't set up in this copy of Kairo.")?;
    let a = account(&server)?;
    let mut inbox = inbox();
    if inbox.account != a.id {
        inbox = Inbox {
            account: a.id.clone(),
            ..Default::default()
        };
    }
    if inbox.me.is_none() {
        let path = format!("/rest/v1/profiles?select=id,username&id=eq.{}", a.id);
        inbox.me = rows::<Profile>(ask(&server, &a, "GET", &path, None, &[])?)?
            .into_iter()
            .next();
        if inbox.me.is_none() {
            save_inbox(&inbox);
            return Ok(inbox);
        }
    }
    inbox.friends = friends(&server, &a)?;
    let after = inbox.messages.last().map_or(0, |m| m.id);
    let path = format!("/rest/v1/messages?select=*&id=gt.{after}&order=id.asc&limit=500");
    inbox.add(rows(ask(&server, &a, "GET", &path, None, &[])?)?);
    save_inbox(&inbox);
    Ok(inbox)
}

fn friends(server: &Server, a: &Account) -> Result<Vec<Friend>, String> {
    #[derive(Deserialize)]
    struct Row {
        requester: String,
        addressee: String,
        accepted: bool,
    }
    let list: Vec<Row> = rows(ask(
        server,
        a,
        "GET",
        "/rest/v1/friendships?select=requester,addressee,accepted",
        None,
        &[],
    )?)?;
    if list.is_empty() {
        return Ok(Vec::new());
    }
    let other = |r: &Row| {
        if r.requester == a.id {
            r.addressee.clone()
        } else {
            r.requester.clone()
        }
    };
    let ids: Vec<String> = list.iter().map(other).collect();
    let path = format!(
        "/rest/v1/profiles?select=id,username&id=in.({})",
        ids.join(",")
    );
    let names: Vec<Profile> = rows(ask(server, a, "GET", &path, None, &[])?)?;
    let mut out: Vec<Friend> = list
        .iter()
        .filter_map(|r| {
            let id = other(r);
            let username = names.iter().find(|p| p.id == id)?.username.clone();
            let standing = match (r.accepted, r.requester == a.id) {
                (true, _) => Standing::Friends,
                (false, true) => Standing::YouAsked,
                (false, false) => Standing::AskedYou,
            };
            Some(Friend {
                id,
                username,
                standing,
            })
        })
        .collect();
    out.sort_by(|x, y| x.username.cmp(&y.username));
    Ok(out)
}

/// A username as stored: trimmed and lower-case. `Err` says what's wrong with it.
pub fn clean_username(name: &str) -> Result<String, String> {
    let name = name.trim().trim_start_matches('@').to_lowercase();
    let ok = (3..=20).contains(&name.chars().count())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(name)
    } else {
        Err("Use 3 to 20 letters, numbers or _ (no spaces).".into())
    }
}

/// Takes `name` as your username.
pub fn choose_username(name: &str) -> Result<(), String> {
    let name = clean_username(name)?;
    let server = server().ok_or("Friends isn't set up in this copy of Kairo.")?;
    let a = account(&server)?;
    let (status, v) = call(
        &server,
        "POST",
        "/rest/v1/profiles",
        Some(&a.token),
        Some(json!({ "username": name })),
        &[("Prefer", "return=minimal")],
    )?;
    match status {
        200..=299 => Ok(()),
        409 => Err(format!("{name} is taken. Try another.")),
        _ => Err(problem(status, &v)),
    }
}

/// Asks `username` to be friends, or accepts if they already asked you. Says what happened.
pub fn add_friend(username: &str) -> Result<String, String> {
    let name = clean_username(username).map_err(|_| "That isn't a Kairo username.".to_string())?;
    let server = server().ok_or("Friends isn't set up in this copy of Kairo.")?;
    let a = account(&server)?;
    let path = format!("/rest/v1/profiles?select=id,username&username=eq.{name}");
    let them = rows::<Profile>(ask(&server, &a, "GET", &path, None, &[])?)?
        .into_iter()
        .next()
        .ok_or_else(|| format!("No one on Kairo is called {name}."))?;
    if them.id == a.id {
        return Err("That's you.".into());
    }
    #[derive(Deserialize)]
    struct Row {
        requester: String,
        accepted: bool,
    }
    let path = format!(
        "/rest/v1/friendships?select=requester,accepted&or=(and(requester.eq.{me},addressee.eq.{them}),and(requester.eq.{them},addressee.eq.{me}))",
        me = a.id,
        them = them.id
    );
    if let Some(r) = rows::<Row>(ask(&server, &a, "GET", &path, None, &[])?)?.first() {
        return if r.accepted {
            Ok(format!("You're already friends with {name}."))
        } else if r.requester == them.id {
            accept_as(&server, &a, &them.id)?;
            Ok(format!("You and {name} are friends now."))
        } else {
            Ok(format!(
                "You already asked {name}. They'll see it in Kairo."
            ))
        };
    }
    ask(
        &server,
        &a,
        "POST",
        "/rest/v1/friendships",
        Some(json!({ "addressee": them.id })),
        &[("Prefer", "return=minimal")],
    )?;
    Ok(format!("Asked {name}. You'll be friends once they accept."))
}

fn accept_as(server: &Server, a: &Account, them: &str) -> Result<(), String> {
    let path = format!(
        "/rest/v1/friendships?requester=eq.{them}&addressee=eq.{}",
        a.id
    );
    ask(
        server,
        a,
        "PATCH",
        &path,
        Some(json!({ "accepted": true })),
        &[("Prefer", "return=minimal")],
    )
    .map(|_| ())
}

/// Accepts `them`'s friend request.
pub fn accept(them: &str) -> Result<(), String> {
    let server = server().ok_or("Friends isn't set up in this copy of Kairo.")?;
    let a = account(&server)?;
    accept_as(&server, &a, them)
}

/// Ends a friendship, or turns down or takes back a request.
pub fn remove(them: &str) -> Result<(), String> {
    let server = server().ok_or("Friends isn't set up in this copy of Kairo.")?;
    let a = account(&server)?;
    let path = format!(
        "/rest/v1/friendships?or=(and(requester.eq.{me},addressee.eq.{them}),and(requester.eq.{them},addressee.eq.{me}))",
        me = a.id
    );
    ask(&server, &a, "DELETE", &path, None, &[]).map(|_| ())
}

/// The rows `send` posts: one per friend.
fn message_rows(to: &[String], body: &str, clip: Option<&ClipLink>) -> Value {
    Value::Array(
        to.iter()
            .map(|id| {
                let mut row = json!({ "recipient": id, "body": body.trim() });
                if let Some(c) = clip {
                    row["clip_url"] = json!(c.url);
                    row["clip_title"] = json!(c.title.chars().take(200).collect::<String>());
                    row["clip_seconds"] = json!(c.seconds);
                }
                row
            })
            .collect(),
    )
}

/// Sends `body`, a clip, or both to each friend in `to`.
pub fn send(to: &[String], body: &str, clip: Option<&ClipLink>) -> Result<(), String> {
    if to.is_empty() {
        return Err("Pick someone to send it to.".into());
    }
    if body.trim().is_empty() && clip.is_none() {
        return Err("Write something first.".into());
    }
    if body.chars().count() > 2000 {
        return Err("That's too long: keep it under 2000 characters.".into());
    }
    if let Some(c) = clip
        && !is_clip_link(&c.url)
    {
        return Err("Only clips shared from Kairo can be sent.".into());
    }
    let server = server().ok_or("Friends isn't set up in this copy of Kairo.")?;
    let a = account(&server)?;
    ask(
        &server,
        &a,
        "POST",
        "/rest/v1/messages",
        Some(message_rows(to, body, clip)),
        &[("Prefer", "return=minimal")],
    )
    .map(|_| ())
}

/// A share link the friends server takes (it checks the same).
pub fn is_clip_link(url: &str) -> bool {
    ["https://files.catbox.moe/", "https://litter.catbox.moe/"]
        .iter()
        .any(|p| {
            url.strip_prefix(p).is_some_and(|rest| {
                !rest.is_empty()
                    && rest
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            })
        })
}

/// Marks `friend`'s messages to you as read.
pub fn mark_read(friend: &str) -> Result<(), String> {
    let server = server().ok_or("Friends isn't set up in this copy of Kairo.")?;
    let a = account(&server)?;
    let path = format!(
        "/rest/v1/messages?sender=eq.{friend}&recipient=eq.{}&read_at=is.null",
        a.id
    );
    let at = chrono::Utc::now().to_rfc3339();
    ask(
        &server,
        &a,
        "PATCH",
        &path,
        Some(json!({ "read_at": at })),
        &[("Prefer", "return=minimal")],
    )?;
    let mut inbox = inbox();
    for m in inbox
        .messages
        .iter_mut()
        .filter(|m| m.sender == friend && m.recipient == a.id && m.read_at.is_none())
    {
        m.read_at = Some(at.clone());
    }
    save_inbox(&inbox);
    Ok(())
}

// --- The background process's part ---------------------------------------------------------

/// The window says which chat it has open (and when), so no pop-up comes for what's on screen.
fn viewing_path() -> PathBuf {
    kairo::config::config_dir().join("friends-viewing.txt")
}

pub fn set_viewing(friend: Option<&str>) {
    match friend {
        Some(f) => {
            let _ = std::fs::write(viewing_path(), format!("{f} {}", now()));
        }
        None => {
            let _ = std::fs::remove_file(viewing_path());
        }
    }
}

fn viewing() -> Option<String> {
    let text = std::fs::read_to_string(viewing_path()).ok()?;
    let (id, at) = text.trim().split_once(' ')?;
    (now() - at.parse::<i64>().ok()? < 20).then(|| id.to_string())
}

/// What pop-ups were shown already, so each message and request gets one.
#[derive(Serialize, Deserialize, Default)]
struct Told {
    message: i64,
    requests: Vec<String>,
}

fn told_path() -> PathBuf {
    kairo::config::config_dir().join("friends-told.json")
}

/// The pop-ups due for what's new in `inbox`: `(from, text, clip)`. Updates `told`.
fn news(inbox: &Inbox, told: &mut Told, viewing: Option<&str>) -> Vec<Vec<String>> {
    let Some(me) = &inbox.me else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let asked: Vec<&Friend> = inbox
        .friends
        .iter()
        .filter(|f| f.standing == Standing::AskedYou && !told.requests.contains(&f.id))
        .collect();
    for f in asked {
        told.requests.push(f.id.clone());
        out.push(vec![
            format!("--from={}", f.username),
            "--text=wants to be your friend on Kairo.".into(),
            "--request".into(),
        ]);
    }
    let fresh: Vec<&Message> = inbox
        .messages
        .iter()
        .filter(|m| {
            m.id > told.message
                && m.recipient == me.id
                && m.read_at.is_none()
                && viewing != Some(m.sender.as_str())
        })
        .collect();
    if let Some(last) = inbox.messages.last() {
        told.message = told.message.max(last.id);
    }
    // A pile-up after the PC was off: the newest few, not one pop-up each.
    let skip = fresh.len().saturating_sub(3);
    for m in &fresh[skip..] {
        let mut args = vec![
            format!("--from={}", inbox.name_of(&m.sender)),
            format!("--text={}", m.preview()),
        ];
        if let Some(url) = &m.clip_url {
            args.push(format!("--link={url}"));
        }
        out.push(args);
    }
    out
}

/// Runs until `shared.stop`: every half minute, fetches what's new and shows the pop-ups.
pub fn run(shared: Arc<Shared>) {
    let mut told: Option<Told> = std::fs::read_to_string(told_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let mut last_error = String::new();
    while !shared.stop.load(Ordering::Relaxed) {
        if available() && signed_in() {
            match sync() {
                Ok(inbox) => {
                    last_error.clear();
                    // The first time: what's there already isn't news.
                    let first = told.is_none();
                    let t = told.get_or_insert_with(Told::default);
                    let due = news(&inbox, t, viewing().as_deref());
                    if let Ok(text) = serde_json::to_string(&*t) {
                        let _ = std::fs::write(told_path(), text);
                    }
                    if !first && shared.settings().notify {
                        for args in due {
                            let mut all = vec!["notify".to_string(), "--friend".into()];
                            all.extend(args);
                            let refs: Vec<&str> = all.iter().map(String::as_str).collect();
                            if let Err(e) = crate::app::spawn_self(&refs) {
                                warning!("{e}");
                            }
                        }
                    }
                }
                Err(e) if e != last_error => {
                    warning!("friends: {e}");
                    last_error = e;
                }
                Err(_) => {}
            }
        }
        let until = Instant::now() + EVERY;
        while Instant::now() < until && !shared.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

// --- Small helpers -------------------------------------------------------------------------

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("the OS has random numbers");
    b
}

/// Base64 for URLs, without padding (PKCE's verifier and challenge).
fn base64url(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 2);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn unpercent(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_the_rfc_example() {
        // RFC 7636, appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            base64url(&Sha256::digest(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(base64url(&random::<48>()).len(), 64);
        assert_eq!(base64url(b"ab"), "YWI");
        assert_eq!(base64url(b"a"), "YQ");
    }

    #[test]
    fn urls_are_encoded_and_decoded() {
        assert_eq!(
            percent(CALLBACK),
            "http%3A%2F%2F127.0.0.1%3A47615%2Fcallback"
        );
        assert_eq!(unpercent("a%20b+c%2Fd%"), "a b c/d%");
        assert_eq!(unpercent("%zz"), "%zz");
    }

    #[test]
    fn usernames_are_lower_case_letters_numbers_and_underscores() {
        assert_eq!(clean_username("  @Fabio_99 "), Ok("fabio_99".into()));
        assert!(clean_username("ab").is_err());
        assert!(clean_username("has space").is_err());
        assert!(clean_username("émile").is_err());
        assert!(clean_username(&"x".repeat(21)).is_err());
    }

    #[test]
    fn only_kairo_share_links_go_as_clips() {
        assert!(is_clip_link("https://files.catbox.moe/ab12cd.mp4"));
        assert!(is_clip_link("https://litter.catbox.moe/x-y_z.mp4"));
        assert!(!is_clip_link("https://files.catbox.moe/"));
        assert!(!is_clip_link("https://evil.example/files.catbox.moe/a.mp4"));
        assert!(!is_clip_link("https://files.catbox.moe/a.mp4?x=<script>"));
    }

    #[test]
    fn a_clip_goes_to_each_friend_as_its_own_row() {
        let clip = ClipLink {
            url: "https://files.catbox.moe/a.mp4".into(),
            title: "Ace".into(),
            seconds: 30.5,
        };
        let v = message_rows(&["a".into(), "b".into()], " gg ", Some(&clip));
        assert_eq!(
            v,
            json!([
                {"recipient": "a", "body": "gg", "clip_url": clip.url, "clip_title": "Ace", "clip_seconds": 30.5},
                {"recipient": "b", "body": "gg", "clip_url": clip.url, "clip_title": "Ace", "clip_seconds": 30.5},
            ])
        );
    }

    fn msg(id: i64, sender: &str, recipient: &str) -> Message {
        Message {
            id,
            sender: sender.into(),
            recipient: recipient.into(),
            body: format!("hi {id}"),
            clip_url: None,
            clip_title: None,
            clip_seconds: None,
            created_at: String::new(),
            read_at: None,
        }
    }

    #[test]
    fn pop_ups_come_once_for_new_messages_and_requests() {
        let mut inbox = Inbox {
            me: Some(Profile {
                id: "me".into(),
                username: "me".into(),
            }),
            account: "me".into(),
            friends: vec![
                Friend {
                    id: "f".into(),
                    username: "fabio".into(),
                    standing: Standing::Friends,
                },
                Friend {
                    id: "n".into(),
                    username: "newbie".into(),
                    standing: Standing::AskedYou,
                },
            ],
            messages: vec![msg(1, "f", "me"), msg(2, "me", "f")],
        };
        let mut told = Told::default();
        let due = news(&inbox, &mut told, None);
        assert_eq!(due.len(), 2, "{due:?}");
        assert!(due[0].contains(&"--from=newbie".into()) && due[0].contains(&"--request".into()));
        assert!(due[1].contains(&"--from=fabio".into()) && due[1].contains(&"--text=hi 1".into()));
        assert!(news(&inbox, &mut told, None).is_empty(), "told once");
        // A new one while that chat is open: no pop-up, and none later either.
        inbox.add(vec![msg(3, "f", "me")]);
        assert!(news(&inbox, &mut told, Some("f")).is_empty());
        assert!(news(&inbox, &mut told, None).is_empty());
        // Ten at once (the PC was off): the newest three.
        inbox.add((4..14).map(|i| msg(i, "f", "me")).collect());
        let due = news(&inbox, &mut told, None);
        assert_eq!(due.len(), 3);
        assert!(due[2].contains(&"--text=hi 13".into()));
    }

    #[test]
    fn the_inbox_keeps_messages_once_in_order_and_counts_unread() {
        let mut inbox = Inbox {
            me: Some(Profile {
                id: "me".into(),
                username: "me".into(),
            }),
            ..Default::default()
        };
        inbox.add(vec![msg(2, "f", "me"), msg(1, "me", "f")]);
        let mut again = msg(2, "f", "me");
        again.read_at = Some("now".into());
        inbox.add(vec![again, msg(3, "f", "me")]);
        let ids: Vec<i64> = inbox.messages.iter().map(|m| m.id).collect();
        assert_eq!(ids, [1, 2, 3]);
        assert_eq!(inbox.unread("f"), 1);
        assert_eq!(inbox.chat("f").count(), 3);
    }

    /// Plays the browser: connects and sends `request`, returns the answer.
    fn browse(port: u16, request: &str) -> String {
        use std::io::Read;
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut answer = String::new();
        let _ = s.read_to_string(&mut answer);
        answer
    }

    fn listener() -> (TcpListener, u16) {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        l.set_nonblocking(true).unwrap();
        let port = l.local_addr().unwrap().port();
        (l, port)
    }

    #[test]
    fn the_browser_hands_the_code_back() {
        let (l, port) = listener();
        let browser = std::thread::spawn(move || {
            let icon = browse(port, "GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n");
            let page = browse(
                port,
                "GET /callback?code=9f3b2c1a-0000-4000-8000-000000000000 HTTP/1.1\r\n\r\n",
            );
            (icon, page)
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let code = wait_for_code(&l, &AtomicBool::new(false), deadline);
        let (icon, page) = browser.join().unwrap();
        assert_eq!(code, Ok("9f3b2c1a-0000-4000-8000-000000000000".into()));
        assert!(icon.starts_with("HTTP/1.1 404"), "{icon}");
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("You're signed in"));
    }

    #[test]
    fn a_refused_sign_in_says_why() {
        let (l, port) = listener();
        let browser = std::thread::spawn(move || {
            browse(
                port,
                "GET /callback?error=access_denied&error_description=The+user+said+%3Cno%3E HTTP/1.1\r\n\r\n",
            )
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let r = wait_for_code(&l, &AtomicBool::new(false), deadline);
        let page = browser.join().unwrap();
        assert_eq!(
            r,
            Err("Google sign-in didn't work: The user said <no>".into())
        );
        assert!(page.contains("The user said &lt;no&gt;"), "escaped: {page}");
        let (l, _) = listener();
        let r = wait_for_code(&l, &AtomicBool::new(true), deadline);
        assert_eq!(r, Err("Cancelled".into()));
    }

    /// A one-request server: answers `status` and `body`, returns what it was sent.
    fn fake_server(
        status: &'static str,
        body: &'static str,
    ) -> (Server, std::thread::JoinHandle<String>) {
        use std::io::Read;
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            // Headers, then as much body as Content-Length says.
            loop {
                let n = s.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text
                        .to_ascii_lowercase()
                        .lines()
                        .find_map(|l| {
                            l.strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if got.len() >= end + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let answer = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(answer.as_bytes()).unwrap();
            String::from_utf8_lossy(&got).to_string()
        });
        let server = Server {
            url: format!("http://127.0.0.1:{port}"),
            key: "sb_publishable_test".into(),
        };
        (server, seen)
    }

    #[test]
    fn requests_carry_the_key_the_token_and_the_rows() {
        let (server, seen) = fake_server("201 Created", "");
        let rows = message_rows(&["f".into()], "gg", None);
        let (status, v) = call(
            &server,
            "POST",
            "/rest/v1/messages",
            Some("user-token"),
            Some(rows.clone()),
            &[("Prefer", "return=minimal")],
        )
        .unwrap();
        assert_eq!((status, v), (201, Value::Null));
        let req = seen.join().unwrap();
        let lower = req.to_ascii_lowercase();
        assert!(req.starts_with("POST /rest/v1/messages HTTP/1.1"), "{req}");
        assert!(lower.contains("apikey: sb_publishable_test"), "{req}");
        assert!(lower.contains("authorization: bearer user-token"), "{req}");
        assert!(lower.contains("prefer: return=minimal"), "{req}");
        let body = &req[req.find("\r\n\r\n").unwrap() + 4..];
        assert_eq!(serde_json::from_str::<Value>(body).unwrap(), rows);
    }

    #[test]
    fn filters_reach_the_server_as_written_and_errors_come_back_in_words() {
        let (server, seen) = fake_server(
            "409 Conflict",
            r#"{"code":"23505","message":"duplicate key value violates unique constraint"}"#,
        );
        let path = "/rest/v1/friendships?select=requester,accepted&or=(and(requester.eq.a,addressee.eq.b),and(requester.eq.b,addressee.eq.a))";
        let (status, v) = call(&server, "GET", path, Some("t"), None, &[]).unwrap();
        let req = seen.join().unwrap();
        assert!(req.starts_with(&format!("GET {path} HTTP/1.1")), "{req}");
        assert_eq!(status, 409);
        assert_eq!(
            problem(status, &v),
            "duplicate key value violates unique constraint"
        );
        assert_eq!(
            problem(503, &Value::Null),
            "Kairo's friends server isn't working right now (error 503)."
        );
        assert_eq!(
            problem(429, &Value::Null),
            "Too many tries at once. Wait a minute and try again."
        );
    }

    #[test]
    fn a_session_keeps_whose_token_it_is() {
        // What Supabase Auth answers for a sign-in or a refresh.
        let v = json!({
            "access_token": "a.b.c",
            "token_type": "bearer",
            "expires_in": 3600,
            "expires_at": now() + 3600,
            "refresh_token": "r1",
            "user": {"id": "u1", "email": "x@example.com", "user_metadata": {"full_name": "X"}}
        });
        let field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        assert_eq!(field("access_token").as_deref(), Some("a.b.c"));
        let s = Stored {
            refresh_token: field("refresh_token").unwrap(),
            user_id: v["user"]["id"].as_str().unwrap().into(),
            email: v["user"]["email"].as_str().map(str::to_string),
        };
        // What goes in Credential Manager stays far under its 2560-byte limit.
        assert!(serde_json::to_string(&s).unwrap().len() < 300);
    }

    #[test]
    fn previews_say_what_was_sent() {
        let mut m = msg(1, "f", "me");
        m.body = String::new();
        m.clip_url = Some("https://files.catbox.moe/a.mp4".into());
        m.clip_title = Some("Ace".into());
        assert_eq!(m.preview(), "Sent a clip: Ace");
        m.body = "look at this\nsecond line".into();
        assert_eq!(m.preview(), "look at this");
    }
}
