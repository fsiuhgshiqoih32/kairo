//! Kairo's look: a dark charcoal UI with one accent, the red of the icon.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use iced::border::Radius;
use iced::font::Weight;
use iced::overlay::menu;
use iced::widget::{
    button, container, pick_list, progress_bar, scrollable, slider, text_input, toggler,
};
use iced::{Background, Border, Color, Font, Shadow, Theme, Vector, color};

/// Light or dark, as picked in Settings (or following the system). Dark until told otherwise.
static LIGHT: AtomicBool = AtomicBool::new(false);

/// Uses the light look (`true`) or the dark one from now on.
pub fn set_light(on: bool) {
    LIGHT.store(on, Ordering::Relaxed);
}

/// Applies a look from the settings: "light", "system" (the computer's choice) or dark.
pub fn apply_look(look: &str) {
    set_light(match look {
        "light" => true,
        "system" => crate::platform::prefers_light().unwrap_or(false),
        _ => false,
    });
}

pub fn is_light() -> bool {
    LIGHT.load(Ordering::Relaxed)
}

fn dark_or_light(dark: Color, light: Color) -> Color {
    if is_light() { light } else { dark }
}

/// The window's ground.
pub fn bg() -> Color {
    dark_or_light(color!(0x141519), color!(0xf4f5f7))
}
pub fn sidebar_bg() -> Color {
    dark_or_light(color!(0x18191e), color!(0xeaecf0))
}
/// Cards, panels and fields.
pub fn surface() -> Color {
    dark_or_light(color!(0x1e2026), color!(0xffffff))
}
pub fn surface_hi() -> Color {
    dark_or_light(color!(0x262930), color!(0xeef0f4))
}
/// Buttons and controls on a surface.
pub fn raised() -> Color {
    dark_or_light(color!(0x2c2f37), color!(0xe2e5eb))
}
/// Hairlines between surfaces.
pub fn edge() -> Color {
    dark_or_light(color!(0x2a2d35), color!(0xd7dbe2))
}
/// Text.
pub fn ink() -> Color {
    dark_or_light(color!(0xeceef1), color!(0x15171c))
}
/// Secondary text.
pub fn ink_2() -> Color {
    dark_or_light(color!(0xa6abb5), color!(0x4a505b))
}
/// Hints and labels.
pub fn muted() -> Color {
    dark_or_light(color!(0x737884), color!(0x6c7380))
}
/// Text and icons drawn on top of a picture: light in either look.
pub const ON_IMAGE: Color = color!(0xeceef1);
pub const GOLD: Color = color!(0xffc53d);
pub const GREEN: Color = color!(0x3dd68c);
pub const RED: Color = color!(0xe5484d);
pub const DISCORD: Color = color!(0x5865f2);

pub const FONT: Font = Font::with_name("Inter");
pub const MEDIUM: Font = Font {
    weight: Weight::Medium,
    ..FONT
};
pub const SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..FONT
};
pub const BOLD: Font = Font {
    weight: Weight::Bold,
    ..FONT
};

pub const FONTS: [&[u8]; 4] = [
    include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/Inter-Bold.ttf"),
];

/// The accent colour picked in Settings (`set_accent`), as 0xRRGGBB.
/// Starts as orange, the default, for windows that never read the settings.
static ACCENT_RGB: AtomicU32 = AtomicU32::new(ACCENT_COLORS[1]);

/// The colours behind `kairo::config::ACCENTS`, in the same order.
pub const ACCENT_COLORS: [u32; 8] = [
    0xff4f46, 0xff7a1a, 0xf5b301, 0x2fcf7f, 0x1fc2e0, 0x3d8bff, 0x8b5cf6, 0xf0469c,
];

/// Uses the accent named `name` (one of `kairo::config::ACCENTS`) from now on.
pub fn set_accent(name: &str) {
    let i = kairo::config::ACCENTS
        .iter()
        .position(|a| *a == name)
        .unwrap_or(0);
    ACCENT_RGB.store(ACCENT_COLORS[i], Ordering::Relaxed);
}

pub fn rgb(v: u32) -> Color {
    Color::from_rgb8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::from_rgb(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
    )
}

/// The accent as 8-bit red, green and blue.
pub fn accent_rgb() -> [u8; 3] {
    let v = ACCENT_RGB.load(Ordering::Relaxed);
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

pub fn accent() -> Color {
    rgb(ACCENT_RGB.load(Ordering::Relaxed))
}

/// The accent, lighter: hovered buttons.
pub fn accent_hi() -> Color {
    mix(accent(), Color::WHITE, 0.14)
}

/// Text on the accent: white, or near-black on the bright ones (gold, green, cyan).
pub fn on_accent() -> Color {
    let c = accent();
    if 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b > 0.6 {
        color!(0x101114)
    } else {
        Color::WHITE
    }
}

pub fn theme() -> Theme {
    Theme::custom(
        "Kairo",
        iced::theme::Palette {
            background: bg(),
            text: ink(),
            primary: accent(),
            success: GREEN,
            warning: GOLD,
            danger: color!(0xe5484d),
        },
    )
}

fn radius(r: f32) -> Radius {
    Radius::from(r)
}

pub fn alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

// --- Containers --------------------------------------------------------------------------

pub fn sidebar(_: &Theme) -> container::Style {
    container::Style {
        background: Some(sidebar_bg().into()),
        border: Border {
            color: edge(),
            width: 0.0,
            radius: radius(0.0),
        },
        ..Default::default()
    }
}

pub fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(surface().into()),
        border: Border {
            color: edge(),
            width: 1.0,
            radius: radius(12.0),
        },
        ..Default::default()
    }
}

pub fn well(_: &Theme) -> container::Style {
    container::Style {
        background: Some(dark_or_light(color!(0x0f1013), color!(0xffffff)).into()),
        border: Border {
            color: edge(),
            width: 1.0,
            radius: radius(10.0),
        },
        ..Default::default()
    }
}

/// A small dark label on top of a thumbnail (duration, "Trimmed").
pub fn badge(_: &Theme) -> container::Style {
    container::Style {
        background: Some(alpha(color!(0x0b0c0f), 0.82).into()),
        text_color: Some(ON_IMAGE),
        border: Border {
            radius: radius(6.0),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A key's outline; the label takes the colour of the button it's on.
pub fn keycap(_: &Theme) -> container::Style {
    container::Style {
        background: Some(alpha(Color::BLACK, if is_light() { 0.06 } else { 0.18 }).into()),
        text_color: None,
        border: Border {
            color: if is_light() {
                alpha(Color::BLACK, 0.22)
            } else {
                alpha(Color::WHITE, 0.4)
            },
            width: 1.0,
            radius: radius(5.0),
        },
        ..Default::default()
    }
}

pub fn toast(_: &Theme) -> container::Style {
    container::Style {
        background: Some(surface().into()),
        border: Border {
            color: raised(),
            width: 1.0,
            radius: radius(12.0),
        },
        shadow: Shadow {
            color: alpha(Color::BLACK, 0.5),
            offset: Vector::new(0.0, 8.0),
            blur_radius: 24.0,
        },
        ..Default::default()
    }
}

// --- Buttons ----------------------------------------------------------------------------

fn base_button(bg: Option<Color>, text: Color, border: Color, r: f32) -> button::Style {
    button::Style {
        background: bg.map(Background::Color),
        text_color: text,
        border: Border {
            color: border,
            width: if border.a > 0.0 { 1.0 } else { 0.0 },
            radius: radius(r),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

/// The main action: filled with the accent.
pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered => button::Style {
            // A soft glow in the accent colour.
            shadow: Shadow {
                color: alpha(accent(), 0.45),
                offset: Vector::new(0.0, 4.0),
                blur_radius: 18.0,
            },
            ..base_button(Some(accent_hi()), on_accent(), Color::TRANSPARENT, 9.0)
        },
        button::Status::Pressed => base_button(
            Some(mix(accent(), Color::BLACK, 0.1)),
            on_accent(),
            Color::TRANSPARENT,
            9.0,
        ),
        button::Status::Disabled => base_button(Some(raised()), muted(), Color::TRANSPARENT, 9.0),
        button::Status::Active => base_button(Some(accent()), on_accent(), Color::TRANSPARENT, 9.0),
    }
}

pub fn discord(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered => base_button(
            Some(color!(0x6d78f5)),
            Color::WHITE,
            Color::TRANSPARENT,
            9.0,
        ),
        button::Status::Disabled => base_button(Some(raised()), muted(), Color::TRANSPARENT, 9.0),
        _ => base_button(Some(DISCORD), Color::WHITE, Color::TRANSPARENT, 9.0),
    }
}

/// An ordinary button: a raised grey surface.
pub fn secondary(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => base_button(
            Some(dark_or_light(color!(0x363a43), color!(0xd3d7de))),
            ink(),
            Color::TRANSPARENT,
            9.0,
        ),
        button::Status::Disabled => base_button(Some(surface()), muted(), Color::TRANSPARENT, 9.0),
        button::Status::Active => base_button(Some(raised()), ink(), Color::TRANSPARENT, 9.0),
    }
}

/// Text-only until hovered.
pub fn ghost(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => {
            base_button(Some(surface_hi()), ink(), Color::TRANSPARENT, 8.0)
        }
        button::Status::Disabled => base_button(None, alpha(muted(), 0.6), Color::TRANSPARENT, 8.0),
        button::Status::Active => base_button(None, ink_2(), Color::TRANSPARENT, 8.0),
    }
}

pub fn danger_ghost(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => base_button(
            Some(alpha(color!(0xe5484d), 0.16)),
            color!(0xff8e88),
            Color::TRANSPARENT,
            8.0,
        ),
        _ => base_button(None, ink_2(), Color::TRANSPARENT, 8.0),
    }
}

/// A sidebar entry; `active` marks the current one.
pub fn nav(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let bg = match (active, status) {
            (true, _) => Some(alpha(accent(), 0.14)),
            (false, button::Status::Hovered | button::Status::Pressed) => Some(surface()),
            _ => None,
        };
        base_button(
            bg,
            if active { ink() } else { ink_2() },
            Color::TRANSPARENT,
            8.0,
        )
    }
}

/// One option of a segmented control.
pub fn segment(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let bg = match (selected, status) {
            (true, _) => Some(raised()),
            (false, button::Status::Hovered) => Some(surface_hi()),
            _ => None,
        };
        base_button(
            bg,
            if selected { ink() } else { ink_2() },
            Color::TRANSPARENT,
            7.0,
        )
    }
}

/// A clip card: no fill of its own; lifts slightly when hovered.
pub fn card(_: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(if hovered { surface_hi() } else { surface() }.into()),
        text_color: ink(),
        border: Border {
            color: if hovered {
                alpha(accent(), 0.6)
            } else {
                edge()
            },
            width: 1.0,
            radius: radius(14.0),
        },
        // Hovered cards glow in the accent colour.
        shadow: if hovered {
            Shadow {
                color: alpha(accent(), 0.28),
                offset: Vector::new(0.0, 8.0),
                blur_radius: 30.0,
            }
        } else {
            Shadow::default()
        },
        snap: true,
    }
}

/// A round icon button on top of a thumbnail (the favourite star).
pub fn overlay_icon(_: &Theme, status: button::Status) -> button::Style {
    let bg = if matches!(status, button::Status::Hovered) {
        0.92
    } else {
        0.7
    };
    base_button(
        Some(alpha(color!(0x0b0c0f), bg)),
        ON_IMAGE,
        Color::TRANSPARENT,
        999.0,
    )
}

// --- Inputs -------------------------------------------------------------------------------

pub fn input(_: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: surface().into(),
        border: Border {
            color: if focused {
                alpha(accent(), 0.7)
            } else {
                edge()
            },
            width: 1.0,
            radius: radius(9.0),
        },
        icon: muted(),
        placeholder: muted(),
        value: ink(),
        selection: alpha(accent(), 0.35),
    }
}

/// The clip name in the editor: plain text until hovered or focused.
pub fn title_input(_: &Theme, status: text_input::Status) -> text_input::Style {
    let (bg, border) = match status {
        text_input::Status::Focused { .. } => (surface(), alpha(accent(), 0.7)),
        text_input::Status::Hovered => (Color::TRANSPARENT, edge()),
        _ => (Color::TRANSPARENT, Color::TRANSPARENT),
    };
    text_input::Style {
        background: bg.into(),
        border: Border {
            color: border,
            width: 1.0,
            radius: radius(8.0),
        },
        icon: muted(),
        placeholder: muted(),
        value: ink(),
        selection: alpha(accent(), 0.35),
    }
}

pub fn scroll(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let mut s = scrollable::default(theme, status);
    for rail in [&mut s.vertical_rail, &mut s.horizontal_rail] {
        rail.background = None;
        rail.border = Border::default();
        rail.scroller.background = raised().into();
        rail.scroller.border = Border {
            radius: radius(999.0),
            ..Default::default()
        };
    }
    s
}

pub fn pick(_: &Theme, status: pick_list::Status) -> pick_list::Style {
    let hovered = matches!(
        status,
        pick_list::Status::Hovered | pick_list::Status::Opened { .. }
    );
    pick_list::Style {
        text_color: ink(),
        placeholder_color: muted(),
        handle_color: ink_2(),
        background: if hovered {
            dark_or_light(color!(0x363a43), color!(0xd3d7de))
        } else {
            raised()
        }
        .into(),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: radius(9.0),
        },
    }
}

pub fn menu(_: &Theme) -> menu::Style {
    menu::Style {
        background: surface_hi().into(),
        border: Border {
            color: raised(),
            width: 1.0,
            radius: radius(10.0),
        },
        text_color: ink(),
        selected_text_color: Color::WHITE,
        selected_background: alpha(accent(), 0.85).into(),
        shadow: Shadow {
            color: alpha(Color::BLACK, 0.5),
            offset: Vector::new(0.0, 8.0),
            blur_radius: 22.0,
        },
    }
}

pub fn toggle(_: &Theme, status: toggler::Status) -> toggler::Style {
    let (on, hovered) = match status {
        toggler::Status::Active { is_toggled } => (is_toggled, false),
        toggler::Status::Hovered { is_toggled } => (is_toggled, true),
        toggler::Status::Disabled { is_toggled } => (is_toggled, false),
    };
    toggler::Style {
        background: if on {
            if hovered { accent_hi() } else { accent() }
        } else if hovered {
            dark_or_light(color!(0x3b3f49), color!(0xcdd2da))
        } else {
            raised()
        }
        .into(),
        background_border_width: 0.0,
        background_border_color: Color::TRANSPARENT,
        foreground: if on { Color::WHITE } else { ink_2() }.into(),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: None,
        border_radius: None,
        padding_ratio: 0.12,
    }
}

pub fn volume_slider(theme: &Theme, status: slider::Status) -> slider::Style {
    let mut s = slider::default(theme, status);
    s.rail.backgrounds = (accent().into(), raised().into());
    s.rail.width = 4.0;
    s.handle.shape = slider::HandleShape::Circle { radius: 7.0 };
    s.handle.background = Color::WHITE.into();
    s.handle.border_width = 0.0;
    s
}

pub fn progress(_: &Theme) -> progress_bar::Style {
    progress_bar::Style {
        background: raised().into(),
        bar: accent().into(),
        border: Border {
            radius: radius(3.0),
            ..Default::default()
        },
    }
}

/// A tinted box for a result line.
pub fn chip_box(c: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(alpha(c, 0.1).into()),
        text_color: Some(ink()),
        border: Border {
            color: alpha(c, 0.3),
            width: 1.0,
            radius: radius(10.0),
        },
        ..Default::default()
    }
}
