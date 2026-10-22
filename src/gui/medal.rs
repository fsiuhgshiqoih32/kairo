//! Importing clips from Medal: looking for them, asking, the copying with its progress, and what
//! came of it. The finding and copying are `kairo::medal`'s.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use iced::keyboard::{Key, key::Named};
use iced::widget::{button, column, container, progress_bar, row, text};
use iced::{Alignment, Element, Fill, Padding, Task};

use kairo::config::Settings;
use kairo::medal::{self, Kind, Plan, Progress, Target};

use super::icons::{Icon, icon};
use super::theme::{self, MEDIUM, SEMIBOLD};
use super::widgets::dialog;
use super::{background, human_size};
use crate::log::{info, warning};

#[derive(Debug, Clone)]
pub enum Msg {
    /// Looks for Medal's clips where Medal keeps them, or in a folder picked by hand.
    Start(Option<PathBuf>),
    /// Asks for the folder Medal keeps its clips in.
    Choose,
    Chosen(Option<PathBuf>),
    Looked(Box<Look>),
    Import,
    /// Closes the dialog; while copying, stops (what's copied stays).
    Cancel,
    /// Closes the dialog and leaves the copying going.
    Hide,
    Tick,
}

/// What a look for Medal's clips found, and what importing them would take.
#[derive(Debug, Clone)]
pub struct Look {
    /// The folder picked by hand, if one was.
    picked: Option<PathBuf>,
    folders: Vec<PathBuf>,
    plan: Plan,
    /// Bytes free on the drive they'd go to.
    free: Option<u64>,
    /// How many Kairo's "keep clips" setting would move to Recently deleted.
    expiring: usize,
    keep_days: u32,
    /// The clips folder's size limit in GB, if they'd take it over.
    over_limit: Option<u32>,
}

impl Look {
    fn fits(&self) -> bool {
        medal::fits(self.plan.bytes(), self.free)
    }
}

enum Stage {
    Looking,
    /// Nothing found where Kairo looked, or in the folder picked.
    Missing(Option<PathBuf>),
    Ask(Box<Look>),
    Copying {
        progress: Arc<Mutex<Progress>>,
        cancel: Arc<AtomicBool>,
    },
}

/// What the import asks of the window.
pub enum Effect {
    None,
    /// Say this (true: it went well).
    Say(String, bool),
    /// It's over: say this, and look at the clips folder again.
    Done(String, bool),
}

pub struct Import {
    stage: Option<Stage>,
    /// The dialog is closed while the copying goes on.
    hidden: bool,
    /// When the dialog opened, for its entrance.
    pub opened: Option<Instant>,
    /// Medal looks to be on this computer, so the empty library offers to import from it.
    pub installed: bool,
}

impl Import {
    pub fn new() -> Import {
        Import {
            stage: None,
            hidden: false,
            opened: None,
            installed: medal::installed(),
        }
    }

    /// The dialog is on screen.
    pub fn showing(&self) -> bool {
        self.stage.is_some() && !self.hidden
    }

    /// Copying, on screen or not.
    pub fn busy(&self) -> bool {
        matches!(self.stage, Some(Stage::Copying { .. }))
    }

    fn show(&mut self, stage: Stage) {
        if !self.showing() {
            self.opened = Some(Instant::now());
        }
        self.stage = Some(stage);
        self.hidden = false;
    }

    pub fn update(&mut self, msg: Msg, settings: &Settings) -> (Task<Msg>, Effect) {
        match msg {
            Msg::Start(folder) => {
                if self.busy() {
                    // Already copying: show how it's going.
                    if self.hidden {
                        self.opened = Some(Instant::now());
                        self.hidden = false;
                    }
                    return (Task::none(), Effect::None);
                }
                self.show(Stage::Looking);
                let settings = settings.clone();
                return (
                    Task::perform(background(move || look(folder, &settings)), |l| {
                        Msg::Looked(Box::new(l))
                    }),
                    Effect::None,
                );
            }
            Msg::Choose => {
                let start = match &self.stage {
                    Some(Stage::Missing(Some(d))) => Some(d.clone()),
                    _ => medal::default_folders().into_iter().find(|d| d.is_dir()),
                };
                return (
                    Task::perform(
                        background(move || {
                            let pick = rfd::FileDialog::new().set_title("Medal's clips folder");
                            match start {
                                Some(d) => pick.set_directory(d),
                                None => pick,
                            }
                            .pick_folder()
                        }),
                        Msg::Chosen,
                    ),
                    Effect::None,
                );
            }
            Msg::Chosen(Some(dir)) => return self.update(Msg::Start(Some(dir)), settings),
            Msg::Chosen(None) => {}
            Msg::Looked(look) => {
                // Closed while it looked: never mind.
                if !matches!(self.stage, Some(Stage::Looking)) {
                    return (Task::none(), Effect::None);
                }
                if look.folders.is_empty() {
                    self.stage = Some(Stage::Missing(look.picked));
                } else if look.plan.copies.is_empty() {
                    self.stage = None;
                    return (
                        Task::none(),
                        Effect::Say("Your Medal clips are all in Kairo already.".into(), true),
                    );
                } else {
                    self.stage = Some(Stage::Ask(look));
                }
            }
            Msg::Import => {
                let Some(Stage::Ask(look)) = &self.stage else {
                    return (Task::none(), Effect::None);
                };
                if !look.fits() {
                    return (Task::none(), Effect::None);
                }
                let progress = Arc::new(Mutex::new(Progress::default()));
                let cancel = Arc::new(AtomicBool::new(false));
                let (plan, to) = (look.plan.clone(), Target::kairo(settings));
                let (p, c) = (progress.clone(), cancel.clone());
                let started = std::thread::Builder::new()
                    .name("medal import".into())
                    .spawn(move || copy(&plan, &to, &p, &c));
                match started {
                    Ok(_) => self.stage = Some(Stage::Copying { progress, cancel }),
                    Err(e) => {
                        self.stage = None;
                        return (
                            Task::none(),
                            Effect::Say(format!("Couldn't start the import: {e}"), false),
                        );
                    }
                }
            }
            Msg::Cancel => match &self.stage {
                // The dialog stays until the copying has stopped.
                Some(Stage::Copying { cancel, .. }) => cancel.store(true, Ordering::Relaxed),
                _ => self.stage = None,
            },
            Msg::Hide => self.hidden = self.busy(),
            Msg::Tick => {
                let Some(Stage::Copying { progress, .. }) = &self.stage else {
                    return (Task::none(), Effect::None);
                };
                let (text, ok) = {
                    let p = progress.lock().unwrap();
                    if !p.finished {
                        return (Task::none(), Effect::None);
                    }
                    p.summary()
                };
                self.stage = None;
                self.hidden = false;
                return (Task::none(), Effect::Done(text, ok));
            }
        }
        (Task::none(), Effect::None)
    }

    /// Escape closes the dialog (leaving any copying going); Enter does what it's there for.
    pub fn key(&self, key: &Key) -> Option<Msg> {
        let stage = self.stage.as_ref()?;
        match key.as_ref() {
            Key::Named(Named::Escape) if self.busy() => Some(Msg::Hide),
            Key::Named(Named::Escape) => Some(Msg::Cancel),
            Key::Named(Named::Enter) => match stage {
                Stage::Ask(look) if look.fits() => Some(Msg::Import),
                Stage::Missing(_) => Some(Msg::Choose),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn view(&self, shown: f32) -> Option<Element<'_, Msg>> {
        if self.hidden {
            return None;
        }
        Some(match self.stage.as_ref()? {
            Stage::Looking => dialog(
                "Import from Medal",
                words("Looking for your Medal clips..."),
                ghost("Cancel", Msg::Cancel),
                Msg::Cancel,
                shown,
            ),
            Stage::Missing(picked) => {
                let (title, body) = match picked {
                    None => (
                        "Medal's clips weren't found",
                        format!(
                            "Kairo looked in Medal's settings and in {}. If your Medal clips are somewhere else, choose that folder.",
                            medal::default_folders()
                                .first()
                                .map_or("Medal's usual folder".into(), |d| d.display().to_string())
                        ),
                    ),
                    Some(d) => (
                        "No Medal clips there",
                        format!(
                            "There are no clips in {}. Choose the folder Medal saves to, the one with Clips and Screenshots in it.",
                            d.display()
                        ),
                    ),
                };
                dialog(
                    title,
                    words(body),
                    row![
                        ghost("Cancel", Msg::Cancel),
                        primary(Icon::Folder, "Choose folder...", Some(Msg::Choose)),
                    ]
                    .spacing(8),
                    Msg::Cancel,
                    shown,
                )
            }
            Stage::Ask(look) => ask(look, shown),
            Stage::Copying { progress, cancel } => {
                let p = progress.lock().unwrap();
                let stopping = cancel.load(Ordering::Relaxed);
                let at = (p.done + 1).min(p.total.max(1));
                let part = if p.total_bytes > 0 {
                    p.bytes as f32 / p.total_bytes as f32
                } else {
                    p.done as f32 / p.total.max(1) as f32
                };
                let body = column![
                    text(if stopping {
                        "Stopping...".to_string()
                    } else {
                        format!("Importing {at} of {}", p.total)
                    })
                    .size(14)
                    .font(MEDIUM)
                    .color(theme::ink()),
                    progress_bar(0.0..=1.0, part.clamp(0.0, 1.0))
                        .girth(6)
                        .style(theme::progress),
                    text(format!(
                        "{} of {} copied. Cancel keeps what's copied so far.",
                        human_size(p.bytes),
                        human_size(p.total_bytes)
                    ))
                    .size(12)
                    .color(theme::muted()),
                ]
                .spacing(10);
                dialog(
                    "Importing from Medal",
                    body,
                    row![
                        ghost("Hide", Msg::Hide),
                        button(text("Cancel").size(13).font(MEDIUM))
                            .padding(Padding::from([8, 14]))
                            .style(theme::secondary)
                            .on_press_maybe((!stopping).then_some(Msg::Cancel)),
                    ]
                    .spacing(8),
                    Msg::Hide,
                    shown,
                )
            }
        })
    }
}

/// "Found 87 Medal clips (23 GB) in D:\Medal. Import them into Kairo?", what to know before, and
/// Import.
fn ask(look: &Look, shown: f32) -> Element<'_, Msg> {
    let plan = &look.plan;
    let (clips, shots) = (plan.count(Kind::Clip), plan.count(Kind::Screenshot));
    let them = if clips + shots == 1 { "it" } else { "them" };
    let what = if plan.already == 0 {
        "Medal"
    } else {
        "new Medal"
    };
    // The folders on lines of their own, so a path isn't broken across two.
    let mut folders = column![].spacing(6);
    for f in &look.folders {
        folders = folders.push(
            row![
                icon(Icon::Folder, 14.0, theme::muted()),
                text(f.display().to_string())
                    .size(13)
                    .font(MEDIUM)
                    .wrapping(text::Wrapping::None),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    let mut body = column![
        words(format!(
            "Found {} ({}) in",
            medal::describe(clips, shots, what),
            human_size(plan.bytes()),
        )),
        container(folders)
            .padding(Padding::from([9, 12]))
            .width(Fill)
            .clip(true)
            .style(theme::well),
        words(format!(
            "Import {them} into Kairo? They're copied; your Medal clips stay where they are."
        )),
    ]
    .spacing(10);
    let mut notes = Vec::new();
    if plan.already > 0 {
        notes.push(format!(
            "{} more {} in Kairo already, so {} left out.",
            plan.already,
            if plan.already == 1 { "is" } else { "are" },
            if plan.already == 1 { "it's" } else { "they're" }
        ));
    }
    if clips > 0 {
        notes.push(format!(
            "Clips go in a folder called {} in your library.",
            medal::FOLDER
        ));
    }
    if look.expiring > 0 {
        notes.push(format!(
            "{} of them are older than {} days, so Keep clips (in Settings) will move them to Recently deleted.",
            look.expiring, look.keep_days
        ));
    }
    if let Some(gb) = look.over_limit {
        notes.push(format!(
            "That's over your {gb} GB limit for clips, so Kairo will clean up the oldest."
        ));
    }
    if !notes.is_empty() {
        body = body.push(notes.into_iter().fold(column![].spacing(6), |c, n| {
            c.push(text(n).size(12).color(theme::muted()))
        }));
    }
    if !look.fits() {
        body = body.push(
            text(format!(
                "That needs {} free, and the drive with your clips folder has {}. Make room, or pick another clips folder in Settings.",
                human_size(plan.bytes() + medal::SPARE_BYTES),
                human_size(look.free.unwrap_or(0))
            ))
            .size(12)
            .color(theme::accent()),
        );
    }
    dialog(
        "Import from Medal",
        body,
        row![
            ghost("Other folder...", Msg::Choose),
            ghost("Cancel", Msg::Cancel),
            primary(Icon::Download, "Import", look.fits().then_some(Msg::Import)),
        ]
        .spacing(8),
        Msg::Cancel,
        shown,
    )
}

/// Looks for Medal's clips (on a worker thread) and works out what importing them takes.
fn look(picked: Option<PathBuf>, settings: &Settings) -> Look {
    let app_data = medal::app_data();
    let found = match &picked {
        Some(dir) => medal::scan_folder(dir, app_data.as_deref()),
        None => medal::find(app_data.as_deref(), &medal::default_folders()),
    };
    let to = Target::kairo(settings);
    let plan = medal::plan(&found, &to);
    info!(
        "Medal: {} files in {} folders, {} new",
        found.items.len(),
        found.folders.len(),
        plan.copies.len()
    );
    let limit = settings.max_storage_gb;
    let used: u64 = kairo::library::scan(&to.clips).iter().map(|c| c.size).sum();
    let adding: u64 = plan
        .copies
        .iter()
        .filter(|(i, _)| i.kind == Kind::Clip)
        .map(|(i, _)| i.size)
        .sum();
    Look {
        picked,
        folders: found.folders,
        free: crate::platform::free_space_for(&to.clips),
        expiring: medal::expiring(&plan, settings.keep_days, SystemTime::now()),
        keep_days: settings.keep_days,
        over_limit: (limit > 0 && used + adding > limit as u64 * 1_000_000_000).then_some(limit),
        plan,
    }
}

/// Copies the files over (on a worker thread), making each clip's picture as it lands.
pub fn copy(plan: &Plan, to: &Target, progress: &Mutex<Progress>, cancel: &AtomicBool) {
    info!(
        "importing {} from Medal",
        medal::describe(plan.count(Kind::Clip), plan.count(Kind::Screenshot), "")
    );
    medal::run(plan, to, progress, cancel, &|item, dest| {
        if item.kind == Kind::Clip {
            crate::save::make_thumb(dest, item.thumb.as_deref());
        }
    });
    let p = progress.lock().unwrap();
    for (path, e) in &p.failed {
        warning!("couldn't import {}: {e}", path.display());
    }
    info!("{}", p.summary().0);
}

fn words<'a>(s: impl Into<String>) -> Element<'a, Msg> {
    text(s.into()).size(14).color(theme::ink_2()).into()
}

fn ghost<'a>(label: &str, msg: Msg) -> Element<'a, Msg> {
    button(text(label.to_string()).size(13).font(MEDIUM))
        .padding(Padding::from([8, 14]))
        .style(theme::ghost)
        .on_press(msg)
        .into()
}

fn primary<'a>(i: Icon, label: &str, msg: Option<Msg>) -> Element<'a, Msg> {
    button(
        row![
            icon(i, 15.0, theme::on_accent()),
            text(label.to_string()).size(13).font(SEMIBOLD)
        ]
        .spacing(7)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([8, 14]))
    .style(theme::primary)
    .on_press_maybe(msg)
    .into()
}

#[cfg(test)]
impl Import {
    /// The dialog as it looks at each step, for the preview: 0 asking (with too little room when
    /// `tight`, and some imported before), 1 copying, 2 nothing found, and otherwise closed.
    pub fn sample(step: usize, tight: bool) -> Import {
        let item = |game: &str, size: u64, kind| medal::Item {
            path: PathBuf::from(format!("D:\\Medal\\Clips\\{game}\\clip.mp4")),
            kind,
            game: game.into(),
            title: None,
            created: SystemTime::now(),
            modified: SystemTime::now() - std::time::Duration::from_secs(86_400 * 120),
            size,
            favorite: false,
            thumb: None,
        };
        let mut plan = Plan::default();
        for i in 0..87 {
            plan.copies.push((
                item("Valorant", 264_000_000, Kind::Clip),
                PathBuf::from(format!("Valorant {i}.mp4")),
            ));
        }
        for _ in 0..13 {
            plan.copies.push((
                item("Valorant", 2_000_000, Kind::Screenshot),
                PathBuf::from("shot.png"),
            ));
        }
        if tight {
            plan.already = 75;
        }
        let stage = match step {
            0 => Some(Stage::Ask(Box::new(Look {
                picked: None,
                folders: vec![PathBuf::from("D:\\Medal"), PathBuf::from("C:\\Medal")],
                free: Some(if tight {
                    9_400_000_000
                } else {
                    410_000_000_000
                }),
                expiring: if tight { 0 } else { 61 },
                keep_days: 90,
                over_limit: None,
                plan,
            }))),
            1 => Some(Stage::Copying {
                progress: Arc::new(Mutex::new(Progress {
                    total: 100,
                    done: 11,
                    bytes: 2_950_000_000,
                    total_bytes: plan.bytes(),
                    clips: 11,
                    ..Default::default()
                })),
                cancel: Arc::new(AtomicBool::new(false)),
            }),
            2 => Some(Stage::Missing(None)),
            _ => None,
        };
        Import {
            stage,
            hidden: false,
            opened: None,
            installed: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn looked(folders: Vec<PathBuf>, plan: Plan) -> Msg {
        Msg::Looked(Box::new(Look {
            picked: None,
            folders,
            plan,
            free: None,
            expiring: 0,
            keep_days: 0,
            over_limit: None,
        }))
    }

    #[test]
    fn looking_asking_copying_and_closing() {
        let s = Settings::default();
        let escape = Key::Named(Named::Escape);
        let mut imp = Import::sample(3, false);
        assert!(!imp.showing());

        // Nothing found: offers to choose the folder.
        imp.stage = Some(Stage::Looking);
        let _ = imp.update(looked(Vec::new(), Plan::default()), &s);
        assert!(matches!(imp.stage, Some(Stage::Missing(None))));
        assert!(matches!(
            imp.key(&Key::Named(Named::Enter)),
            Some(Msg::Choose)
        ));
        assert!(matches!(imp.key(&escape), Some(Msg::Cancel)));
        let _ = imp.update(Msg::Cancel, &s);
        assert!(!imp.showing());

        // Found, but all in Kairo already: just says so.
        imp.stage = Some(Stage::Looking);
        let plan = Plan {
            already: 3,
            ..Default::default()
        };
        let (_, said) = imp.update(looked(vec![PathBuf::from("D:\\Medal")], plan), &s);
        assert!(matches!(said, Effect::Say(_, true)));
        assert!(imp.stage.is_none());
        // A look that ends after the dialog was closed changes nothing.
        let (_, said) = imp.update(looked(Vec::new(), Plan::default()), &s);
        assert!(matches!(said, Effect::None) && imp.stage.is_none());

        // Copying: Escape and the backdrop hide it, it goes on, and asking again shows it.
        let mut imp = Import::sample(1, false);
        assert!(matches!(imp.key(&escape), Some(Msg::Hide)));
        let _ = imp.update(Msg::Hide, &s);
        assert!(imp.busy() && !imp.showing());
        assert!(imp.view(1.0).is_none());
        let _ = imp.update(Msg::Start(None), &s);
        assert!(imp.busy() && imp.showing());
        // Cancel asks it to stop; the dialog stays until it has.
        let _ = imp.update(Msg::Cancel, &s);
        let Some(Stage::Copying { progress, cancel }) = &imp.stage else {
            panic!("not copying");
        };
        assert!(cancel.load(Ordering::Relaxed));
        let progress = progress.clone();
        let (_, done) = imp.update(Msg::Tick, &s);
        assert!(matches!(done, Effect::None) && imp.busy());
        {
            let mut p = progress.lock().unwrap();
            p.cancelled = true;
            p.finished = true;
        }
        let (_, done) = imp.update(Msg::Tick, &s);
        assert!(
            matches!(&done, Effect::Done(t, true) if t.starts_with("Stopped. Imported 11 clips"))
        );
        assert!(imp.stage.is_none() && !imp.showing());
    }
}
