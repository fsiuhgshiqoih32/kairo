//! The Friends page: sign in with Google, pick a username, add friends by theirs, and chat:
//! messages and clips both ways. `crate::friends` talks to the server; the background process
//! checks for news and shows pop-ups while this page is closed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Element, Fill, Length, Padding, Task};

use crate::friends::{self as net, Friend, Inbox, Standing};

use super::background;
use super::icons::{Icon, icon};
use super::theme::{self, BOLD, MEDIUM, SEMIBOLD};
use super::widgets::{divider, panel};

/// How often the open page asks what's new.
const EVERY: Duration = Duration::from_secs(10);

pub struct Friends {
    pub inbox: Inbox,
    signed_in: bool,
    /// While the browser is open on Google; storing `true` stops the wait.
    signing_in: Option<Arc<AtomicBool>>,
    username: String,
    add: String,
    /// The friend whose chat is open.
    chat: Option<String>,
    draft: String,
    busy: bool,
    /// A line under the controls: what happened, or what went wrong (`false`).
    note: Option<(String, bool)>,
    syncing: bool,
    synced: Option<Instant>,
    read_at: Instant,
    viewing_at: Instant,
}

#[derive(Debug, Clone)]
pub enum Msg {
    SignIn,
    CancelSignIn,
    SignedIn(Result<(), String>),
    SignOut,
    Username(String),
    SaveUsername,
    Add(String),
    AddFriend,
    Added(Result<String, String>),
    Accept(String),
    Remove(String),
    Open(String),
    Close,
    Draft(String),
    Send,
    Sent(Result<(), String>),
    /// Anything else that finished: what to say about it.
    Done(Result<String, String>),
    Synced(Result<Inbox, String>),
    Watch(String),
}

impl Friends {
    pub fn new() -> Friends {
        Friends {
            inbox: net::inbox(),
            signed_in: net::signed_in(),
            signing_in: None,
            username: String::new(),
            add: String::new(),
            chat: None,
            draft: String::new(),
            busy: false,
            note: None,
            syncing: false,
            synced: None,
            read_at: Instant::now(),
            viewing_at: Instant::now(),
        }
    }

    /// For the previews: signed in or not, with `inbox`, on `chat`.
    #[cfg(test)]
    pub fn sample(signed_in: bool, inbox: Inbox, chat: Option<&str>) -> Friends {
        Friends {
            inbox,
            signed_in,
            chat: chat.map(str::to_string),
            synced: Some(Instant::now()),
            ..Friends::new()
        }
    }

    /// Signed in with a username and at least one friend: clips can be sent.
    pub fn can_send(&self) -> bool {
        self.signed_in && self.inbox.me.is_some() && self.inbox.friends_only().next().is_some()
    }

    /// Messages to you not yet opened, from everyone.
    pub fn unread(&self) -> usize {
        self.inbox
            .friends_only()
            .map(|f| self.inbox.unread(&f.id))
            .sum()
    }

    /// Requests waiting for your answer.
    pub fn requests(&self) -> usize {
        self.inbox
            .friends
            .iter()
            .filter(|f| f.standing == Standing::AskedYou)
            .count()
    }

    /// Once a second: on the page, fetch what's new every few seconds; elsewhere, pick up what
    /// the background process fetched (for the sidebar's count).
    pub fn tick(&mut self, on_page: bool) -> Task<Msg> {
        if on_page {
            // Refreshed while a chat is open (it counts for 20 s), so no pop-up comes for it.
            if self.chat.is_some() && self.viewing_at.elapsed() >= Duration::from_secs(5) {
                self.viewing_at = Instant::now();
                net::set_viewing(self.chat.as_deref());
            }
            if self.signed_in && !self.syncing && self.synced.is_none_or(|t| t.elapsed() >= EVERY) {
                return self.sync_now();
            }
        } else if self.read_at.elapsed() >= Duration::from_secs(5) {
            self.read_at = Instant::now();
            self.signed_in = net::signed_in();
            if !self.syncing {
                self.inbox = net::inbox();
            }
        }
        Task::none()
    }

    /// Leaving the page: pop-ups come again for every chat.
    pub fn left(&mut self) {
        net::set_viewing(None);
    }

    fn sync_now(&mut self) -> Task<Msg> {
        self.syncing = true;
        self.synced = Some(Instant::now());
        Task::perform(background(net::sync), Msg::Synced)
    }

    pub fn update(&mut self, m: Msg) -> Task<Msg> {
        match m {
            Msg::SignIn => {
                let cancel = Arc::new(AtomicBool::new(false));
                self.signing_in = Some(cancel.clone());
                self.note = None;
                return Task::perform(background(move || net::sign_in(&cancel)), Msg::SignedIn);
            }
            Msg::CancelSignIn => {
                if let Some(c) = &self.signing_in {
                    c.store(true, Ordering::Relaxed);
                }
            }
            Msg::SignedIn(r) => {
                self.signing_in = None;
                match r {
                    Ok(()) => {
                        self.signed_in = true;
                        return self.sync_now();
                    }
                    Err(e) if e == "Cancelled" => {}
                    Err(e) => self.note = Some((e, false)),
                }
            }
            Msg::SignOut => {
                net::sign_out();
                *self = Friends::new();
            }
            Msg::Username(s) => self.username = s.to_lowercase(),
            Msg::SaveUsername => {
                let name = self.username.clone();
                return self.run(move || net::choose_username(&name).map(|()| String::new()));
            }
            Msg::Add(s) => self.add = s,
            Msg::AddFriend => {
                let name = self.add.clone();
                self.busy = true;
                self.note = None;
                return Task::perform(background(move || net::add_friend(&name)), Msg::Added);
            }
            Msg::Added(r) => {
                self.busy = false;
                match r {
                    Ok(said) => {
                        self.add.clear();
                        self.note = Some((said, true));
                        return self.sync_now();
                    }
                    Err(e) => self.note = Some((e, false)),
                }
            }
            Msg::Accept(id) => {
                let name = self.inbox.name_of(&id);
                return self.run(move || {
                    net::accept(&id).map(|()| format!("You and {name} are friends now."))
                });
            }
            Msg::Remove(id) => {
                if self.chat.as_deref() == Some(id.as_str()) {
                    self.chat = None;
                }
                return self.run(move || net::remove(&id).map(|()| String::new()));
            }
            Msg::Open(id) => {
                self.chat = Some(id.clone());
                self.draft.clear();
                self.note = None;
                net::set_viewing(Some(&id));
                if self.inbox.unread(&id) > 0 {
                    return self.run(move || net::mark_read(&id).map(|()| String::new()));
                }
            }
            Msg::Close => {
                self.chat = None;
                self.note = None;
                net::set_viewing(None);
            }
            Msg::Draft(s) => self.draft = s,
            Msg::Send => {
                let (Some(to), body) = (self.chat.clone(), self.draft.clone()) else {
                    return Task::none();
                };
                if body.trim().is_empty() || self.busy {
                    return Task::none();
                }
                self.busy = true;
                return Task::perform(background(move || net::send(&[to], &body, None)), Msg::Sent);
            }
            Msg::Sent(r) => {
                self.busy = false;
                match r {
                    Ok(()) => {
                        self.draft.clear();
                        self.note = None;
                        return self.sync_now();
                    }
                    Err(e) => self.note = Some((e, false)),
                }
            }
            Msg::Done(r) => {
                self.busy = false;
                match r {
                    Ok(said) => {
                        self.note = (!said.is_empty()).then_some((said, true));
                        return self.sync_now();
                    }
                    Err(e) => self.note = Some((e, false)),
                }
            }
            Msg::Synced(r) => {
                self.syncing = false;
                match r {
                    Ok(inbox) => self.inbox = inbox,
                    Err(e) => {
                        self.signed_in = net::signed_in();
                        if self.signed_in {
                            self.note = Some((e, false));
                        }
                    }
                }
            }
            Msg::Watch(url) => {
                if net::is_clip_link(&url) {
                    let _ = opener::open_browser(&url);
                }
            }
        }
        Task::none()
    }

    /// Runs `f` in the background and reports with `Done`.
    fn run(&mut self, f: impl FnOnce() -> Result<String, String> + Send + 'static) -> Task<Msg> {
        self.busy = true;
        self.note = None;
        Task::perform(background(f), Msg::Done)
    }

    pub fn view(&self) -> Element<'_, Msg> {
        if !net::available() {
            return centered(
                Icon::Send,
                "Friends isn't set up here",
                "This copy of Kairo has no friends server. The one from the website does.",
                None,
            );
        }
        if !self.signed_in {
            return self.sign_in_view();
        }
        let Some(me) = &self.inbox.me else {
            return if self.synced.is_none() || self.syncing {
                centered(Icon::Refresh, "Loading your friends", "", None)
            } else {
                self.username_view()
            };
        };
        if let Some(id) = &self.chat
            && let Some(f) = self.inbox.friends.iter().find(|f| &f.id == id)
        {
            return self.chat_view(f);
        }
        let mut body = column![self.add_view(&me.username)].spacing(16);
        let asked: Vec<&Friend> = self
            .inbox
            .friends
            .iter()
            .filter(|f| f.standing != Standing::Friends)
            .collect();
        if !asked.is_empty() {
            let mut list = column![].spacing(10);
            for f in asked {
                let (line, buttons): (String, Element<'_, Msg>) = match f.standing {
                    Standing::AskedYou => (
                        "wants to be your friend".into(),
                        row![
                            small_button("Accept", Msg::Accept(f.id.clone()), true, self.busy),
                            small_button("Decline", Msg::Remove(f.id.clone()), false, self.busy),
                        ]
                        .spacing(6)
                        .into(),
                    ),
                    _ => (
                        "hasn't answered yet".into(),
                        small_button("Take back", Msg::Remove(f.id.clone()), false, self.busy)
                            .into(),
                    ),
                };
                list = list.push(
                    row![
                        avatar(&f.username),
                        column![
                            text(f.username.clone()).font(SEMIBOLD).size(14),
                            text(line).size(12).color(theme::muted()),
                        ]
                        .spacing(2)
                        .width(Fill),
                        buttons,
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                );
            }
            body = body.push(panel("Friend requests", list));
        }
        let mut list = column![].spacing(4);
        let mut any = false;
        for f in self.inbox.friends_only() {
            any = true;
            let last = self.inbox.chat(&f.id).last();
            let unread = self.inbox.unread(&f.id);
            let mut r = row![
                avatar(&f.username),
                column![
                    text(f.username.clone()).font(SEMIBOLD).size(14),
                    container(
                        text(last.map_or_else(
                            || "Say hi, or send a clip".to_string(),
                            |m| {
                                let who = if m.sender == f.id { "" } else { "You: " };
                                format!("{who}{}", m.preview())
                            }
                        ))
                        .size(12)
                        .color(if unread > 0 {
                            theme::ink()
                        } else {
                            theme::muted()
                        })
                        .wrapping(text::Wrapping::None)
                    )
                    .clip(true),
                ]
                .spacing(2)
                .width(Fill),
            ]
            .spacing(12)
            .align_y(Alignment::Center);
            if unread > 0 {
                r = r.push(
                    container(
                        text(unread.to_string())
                            .size(11)
                            .font(BOLD)
                            .color(theme::on_accent()),
                    )
                    .padding(Padding::from([2, 7]))
                    .style(|_| container::Style {
                        background: Some(theme::accent().into()),
                        border: iced::Border {
                            radius: 999.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                );
            }
            list = list.push(
                button(r)
                    .width(Fill)
                    .padding(Padding::from([8, 10]))
                    .style(theme::ghost)
                    .on_press(Msg::Open(f.id.clone())),
            );
        }
        if !any {
            list = list.push(
                text("No friends yet. Add one by their username above; they add you by yours.")
                    .size(13)
                    .color(theme::muted()),
            );
        }
        body = body.push(panel("Friends", list));
        scrollable(container(body.max_width(720)).padding(Padding {
            top: 0.0,
            right: 28.0,
            bottom: 32.0,
            left: 28.0,
        }))
        .style(theme::scroll)
        .height(Fill)
        .into()
    }

    fn sign_in_view(&self) -> Element<'_, Msg> {
        let action: Element<'_, Msg> = if self.signing_in.is_some() {
            column![
                text("Finish signing in in your browser...")
                    .size(14)
                    .color(theme::ink_2()),
                small_button("Cancel", Msg::CancelSignIn, false, false),
            ]
            .spacing(10)
            .align_x(Alignment::Center)
            .into()
        } else {
            button(
                row![
                    text("G").font(BOLD).size(16),
                    text("Sign in with Google").font(SEMIBOLD).size(14)
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([11, 18]))
            .style(theme::primary)
            .on_press(Msg::SignIn)
            .into()
        };
        let mut c = column![
            container(icon(Icon::Send, 30.0, theme::on_accent()))
                .center(Length::Fixed(64.0))
                .style(|_| container::Style {
                    background: Some(theme::accent().into()),
                    border: iced::Border {
                        radius: 32.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            text("Send clips to your friends").font(BOLD).size(22),
            text(
                "Sign in, pick a username and add friends by theirs. Clips and messages you \
                 send show up in their Kairo, with a pop-up."
            )
            .size(14)
            .color(theme::ink_2())
            .align_x(Alignment::Center),
            action,
        ]
        .spacing(14)
        .align_x(Alignment::Center)
        .max_width(460);
        if let Some((n, ok)) = &self.note {
            c = c.push(note(n, *ok));
        }
        c = c.push(
            text(
                "Kairo keeps your username, friends and messages on its server. A clip you send \
                 is uploaded to catbox.moe, where anyone with its link can watch it.",
            )
            .size(12)
            .color(theme::muted())
            .align_x(Alignment::Center),
        );
        container(c).center(Fill).padding(28).into()
    }

    fn username_view(&self) -> Element<'_, Msg> {
        let save = (!self.busy && !self.username.trim().is_empty()).then_some(Msg::SaveUsername);
        let mut c = column![
            text("Pick a username").font(BOLD).size(22),
            text("Friends add you by it. 3 to 20 letters, numbers or _.")
                .size(14)
                .color(theme::ink_2()),
            row![
                text_input("username", &self.username)
                    .on_input(Msg::Username)
                    .on_submit_maybe(save.clone())
                    .padding(Padding::from([9, 12]))
                    .size(15)
                    .style(theme::input),
                small_button("Save", Msg::SaveUsername, true, save.is_none()),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(12)
        .max_width(420);
        if let Some((n, ok)) = &self.note {
            c = c.push(note(n, *ok));
        }
        c = c.push(small_button("Sign out", Msg::SignOut, false, false));
        container(c).center(Fill).padding(28).into()
    }

    fn add_view<'a>(&'a self, me: &str) -> Element<'a, Msg> {
        let add = (!self.busy && !self.add.trim().is_empty()).then_some(Msg::AddFriend);
        let who = match net::email() {
            Some(e) => format!("You're {me} ({e}). Friends add you by {me}."),
            None => format!("You're {me}. Friends add you by that name."),
        };
        let mut c = column![
            row![
                text(who).size(13).color(theme::ink_2()).width(Fill),
                small_button("Sign out", Msg::SignOut, false, false),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text_input("Their username", &self.add)
                    .on_input(Msg::Add)
                    .on_submit_maybe(add.clone())
                    .padding(Padding::from([9, 12]))
                    .size(14)
                    .style(theme::input),
                small_button("Add friend", Msg::AddFriend, true, add.is_none()),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(12);
        if let Some((n, ok)) = &self.note {
            c = c.push(note(n, *ok));
        }
        panel("Add a friend", c)
    }

    fn chat_view<'a>(&'a self, f: &'a Friend) -> Element<'a, Msg> {
        let top = row![
            button(icon(Icon::Back, 18.0, theme::ink()))
                .padding(6)
                .style(theme::ghost)
                .on_press(Msg::Close),
            avatar(&f.username),
            text(f.username.clone()).font(BOLD).size(18).width(Fill),
            small_button("Remove friend", Msg::Remove(f.id.clone()), false, self.busy),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let mut list = column![].spacing(8).padding(Padding::from([8, 0]));
        let mut any = false;
        for m in self.inbox.chat(&f.id) {
            any = true;
            let mine = m.sender != f.id;
            let mut inner = column![].spacing(6);
            if let Some(url) = &m.clip_url {
                let title = m.clip_title.clone().unwrap_or_else(|| "A clip".into());
                let length = m
                    .clip_seconds
                    .map(|s| super::duration(Some(s)))
                    .unwrap_or_default();
                inner = inner.push(
                    row![
                        container(icon(Icon::Film, 18.0, theme::ink()))
                            .center(Length::Fixed(38.0))
                            .style(theme::well),
                        column![
                            text(title).font(SEMIBOLD).size(13),
                            text(length).size(11).color(theme::muted()),
                        ]
                        .spacing(2)
                        .width(Length::Shrink),
                        small_button("Watch", Msg::Watch(url.clone()), true, false),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                );
            }
            if !m.body.trim().is_empty() {
                inner = inner.push(text(m.body.clone()).size(14));
            }
            inner = inner.push(text(when(&m.created_at)).size(10).color(theme::muted()));
            let bubble = container(inner)
                .padding(Padding::from([8, 12]))
                .max_width(460)
                .style(move |_| container::Style {
                    background: Some(if mine {
                        theme::alpha(theme::accent(), 0.18).into()
                    } else {
                        theme::surface_hi().into()
                    }),
                    border: iced::Border {
                        radius: 12.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                });
            list = list.push(if mine {
                row![Space::new().width(Fill), bubble]
            } else {
                row![bubble, Space::new().width(Fill)]
            });
        }
        if !any {
            list = list.push(
                container(
                    text(format!(
                        "Nothing yet. Say hi, or send {} a clip from its card in your library.",
                        f.username
                    ))
                    .size(13)
                    .color(theme::muted()),
                )
                .width(Fill)
                .padding(30)
                .align_x(Alignment::Center),
            );
        }
        let send = (!self.busy && !self.draft.trim().is_empty()).then_some(Msg::Send);
        let mut bottom = column![
            row![
                text_input(&format!("Message {}", f.username), &self.draft)
                    .on_input(Msg::Draft)
                    .on_submit_maybe(send.clone())
                    .padding(Padding::from([10, 12]))
                    .size(14)
                    .style(theme::input),
                button(icon(Icon::Send, 16.0, theme::on_accent()))
                    .padding(Padding::from([10, 12]))
                    .style(theme::primary)
                    .on_press_maybe(send),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(6);
        if let Some((n, ok)) = &self.note {
            bottom = bottom.push(note(n, *ok));
        }
        container(
            column![
                top,
                divider(),
                scrollable(list)
                    .anchor_bottom()
                    .height(Fill)
                    .style(theme::scroll),
                bottom,
            ]
            .spacing(10)
            .max_width(760),
        )
        .padding(Padding {
            top: 0.0,
            right: 28.0,
            bottom: 20.0,
            left: 28.0,
        })
        .height(Fill)
        .into()
    }
}

fn centered<'a>(
    i: Icon,
    title: &'a str,
    line: &'a str,
    action: Option<Element<'a, Msg>>,
) -> Element<'a, Msg> {
    let mut c = column![
        icon(i, 28.0, theme::muted()),
        text(title).font(BOLD).size(18),
        text(line).size(13).color(theme::ink_2()),
    ]
    .spacing(10)
    .align_x(Alignment::Center);
    if let Some(a) = action {
        c = c.push(a);
    }
    container(c).center(Fill).into()
}

/// A circle with the first letter of a name.
fn avatar<'a>(name: &str) -> Element<'a, Msg> {
    let letter = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    container(text(letter).font(BOLD).size(14).color(theme::on_accent()))
        .center(Length::Fixed(34.0))
        .style(|_| container::Style {
            background: Some(theme::alpha(theme::accent(), 0.85).into()),
            border: iced::Border {
                radius: 17.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

fn small_button<'a>(
    label: &'a str,
    m: Msg,
    primary: bool,
    disabled: bool,
) -> iced::widget::Button<'a, Msg> {
    button(text(label).size(13).font(MEDIUM))
        .padding(Padding::from([7, 12]))
        .style(if primary {
            theme::primary
        } else {
            theme::secondary
        })
        .on_press_maybe((!disabled).then_some(m))
}

fn note<'a>(line: &str, ok: bool) -> Element<'a, Msg> {
    text(line.to_string())
        .size(12)
        .color(if ok { theme::GREEN } else { theme::accent() })
        .into()
}

/// When a message was sent, in local time: "14:05" today, "Tue 14:05" this week, else the date.
fn when(at: &str) -> String {
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(at) else {
        return String::new();
    };
    let t = t.with_timezone(&chrono::Local);
    let now = chrono::Local::now();
    let days = (now.date_naive() - t.date_naive()).num_days();
    match days {
        0 => t.format("%H:%M").to_string(),
        1..=6 => t.format("%a %H:%M").to_string(),
        _ => t.format("%b %-d, %H:%M").to_string(),
    }
}
