//! Small pieces the pages share: buttons with icons, segmented choices, panels, dialogs.

use std::sync::{LazyLock, Mutex};

use iced::widget::{Button, Space, button, column, container, image, row, text};
use iced::{Alignment, Color, Element, Fill, Length, Padding};

use super::icons::{Icon, icon};
use super::theme::{self, BOLD, MEDIUM, SEMIBOLD};

/// Kairo's icon at `size` pixels, made once. A picture handle made anew in `view` is a new
/// image to iced every time, decoded and uploaded again, and blinks: the sidebar's logo did,
/// every time the window redrew.
pub fn app_logo(size: u32) -> image::Handle {
    static MADE: Mutex<Vec<(u32, image::Handle)>> = Mutex::new(Vec::new());
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, h)) = made.iter().find(|(s, _)| *s == size) {
        return h.clone();
    }
    let h = image::Handle::from_rgba(size, size, kairo::art::app_icon(size as usize));
    made.push((size, h.clone()));
    h
}

/// The sidebar's logo (from `assets/icon.png`), made once; see [`app_logo`].
pub fn sidebar_logo() -> image::Handle {
    static LOGO: LazyLock<image::Handle> = LazyLock::new(|| {
        image::Handle::from_bytes(include_bytes!("../../assets/icon.png").as_slice())
    });
    LOGO.clone()
}

/// A button with an icon and a label.
pub fn icon_button<'a, M: Clone + 'a>(i: Icon, label: &str, color: Color) -> Button<'a, M> {
    button(
        row![
            icon(i, 15.0, color),
            text(label.to_string()).size(13).font(MEDIUM)
        ]
        .spacing(7)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([7, 11]))
}

/// A square button with just an icon.
pub fn icon_only<'a, M: Clone + 'a>(i: Icon, color: Color) -> Button<'a, M> {
    button(container(icon(i, 16.0, color)).center(Length::Fixed(18.0))).padding(7)
}

/// One-of-several choice: a row of buttons, the chosen one raised.
pub fn segmented<'a, T, M>(
    options: Vec<(T, String)>,
    selected: T,
    on: impl Fn(T) -> M,
) -> Element<'a, M>
where
    T: PartialEq + Copy + 'a,
    M: Clone + 'a,
{
    let mut r = row![].spacing(2);
    for (value, label) in options {
        let chosen = value == selected;
        r = r.push(
            button(
                text(label)
                    .size(13)
                    .font(if chosen { SEMIBOLD } else { MEDIUM }),
            )
            .padding(Padding::from([6, 12]))
            .style(theme::segment(chosen))
            .on_press(on(value)),
        );
    }
    container(r).padding(3).style(theme::well).into()
}

/// A key as it's printed on the keyboard: "F8", "Ctrl".
pub fn keycap<'a, M: 'a>(label: &str) -> Element<'a, M> {
    container(text(label.to_string()).size(12).font(SEMIBOLD))
        .padding(Padding::from([1, 6]))
        .style(theme::keycap)
        .into()
}

/// A muted keycap for hint lines.
pub fn hint_key<'a, M: 'a>(label: &str) -> Element<'a, M> {
    container(
        text(label.to_string())
            .size(11)
            .font(SEMIBOLD)
            .color(theme::ink_2()),
    )
    .padding(Padding::from([1, 5]))
    .style(|_| container::Style {
        border: iced::Border {
            color: theme::raised(),
            width: 1.0,
            radius: 4.0.into(),
        },
        ..Default::default()
    })
    .into()
}

/// A titled card.
pub fn panel<'a, M: 'a>(title: &str, body: impl Into<Element<'a, M>>) -> Element<'a, M> {
    container(column![text(title.to_string()).font(BOLD).size(15), body.into()].spacing(12))
        .padding(16)
        .width(Fill)
        .style(theme::panel)
        .into()
}

/// A thin line between rows.
pub fn divider<'a, M: 'a>() -> Element<'a, M> {
    container(Space::new().width(Fill).height(1))
        .style(|_| container::Style {
            background: Some(theme::edge().into()),
            ..Default::default()
        })
        .into()
}

/// A modal: dims everything and shows `body` in a card in the middle. `shown` runs from 0 to 1
/// as it appears: the backdrop darkens and the card rises into place.
pub fn dialog<'a, M: Clone + 'a>(
    title: &str,
    body: impl Into<Element<'a, M>>,
    actions: impl Into<Element<'a, M>>,
    on_dismiss: M,
    shown: f32,
) -> Element<'a, M> {
    let shown = shown.clamp(0.0, 1.0);
    let card = container(
        column![
            text(title.to_string()).font(BOLD).size(18),
            body.into(),
            container(actions.into())
                .width(Fill)
                .align_x(iced::alignment::Horizontal::Right),
        ]
        .spacing(16),
    )
    .padding(22)
    .max_width(460)
    .style(theme::toast);
    iced::widget::opaque(
        iced::widget::mouse_area(
            container(iced::widget::opaque(card))
                .center(Fill)
                .padding(Padding {
                    top: 20.0 + (1.0 - shown) * 36.0,
                    ..Padding::new(20.0)
                })
                .style(move |_| container::Style {
                    background: Some(
                        Color {
                            a: 0.6 * shown,
                            ..Color::BLACK
                        }
                        .into(),
                    ),
                    ..Default::default()
                }),
        )
        .on_press(on_dismiss),
    )
}

/// Label and a line of explanation, for settings rows.
pub fn label<'a, M: 'a>(title: &str, note: Option<String>) -> Element<'a, M> {
    let mut c = column![
        text(title.to_string())
            .size(14)
            .font(MEDIUM)
            .color(theme::ink())
    ]
    .spacing(3);
    if let Some(n) = note {
        c = c.push(text(n).size(12).color(theme::muted()));
    }
    c.into()
}
