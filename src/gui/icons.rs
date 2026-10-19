//! Line icons (Lucide, ISC licence; see assets/icons/LICENSE.txt), tinted to any colour.

use iced::widget::{Svg, svg};
use iced::{Color, Length};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    Star,
    Folder,
    Copy,
    Trash,
    Search,
    Settings,
    Clips,
    Gamepad,
    Scissors,
    Send,
    Back,
    Volume,
    Mute,
    Close,
    Repeat,
    External,
    Pencil,
    Download,
    Power,
    Log,
    Refresh,
    Film,
    Check,
    Camera,
    Link,
    Qr,
    Mic,
    Image,
    Phone,
    Snail,
    Share,
    Globe,
    Flag,
    Layers,
    ChevronDown,
    Selected,
    Square,
    Portrait,
    Music,
    Sparkles,
    Record,
    Tag,
    Undo,
    Redo,
    Trophy,
    Flame,
    Lock,
    Clock,
}

fn source(icon: Icon) -> &'static [u8] {
    macro_rules! file {
        ($name:literal) => {
            include_bytes!(concat!("../../assets/icons/", $name, ".svg"))
        };
    }
    match icon {
        Icon::Play => file!("play"),
        Icon::Pause => file!("pause"),
        Icon::Star => file!("star"),
        Icon::Folder => file!("folder-open"),
        Icon::Copy => file!("copy"),
        Icon::Trash => file!("trash-2"),
        Icon::Search => file!("search"),
        Icon::Settings => file!("settings"),
        Icon::Clips => file!("clapperboard"),
        Icon::Gamepad => file!("gamepad-2"),
        Icon::Scissors => file!("scissors"),
        Icon::Send => file!("send"),
        Icon::Back => file!("arrow-left"),
        Icon::Volume => file!("volume-2"),
        Icon::Mute => file!("volume-x"),
        Icon::Close => file!("x"),
        Icon::Repeat => file!("repeat"),
        Icon::External => file!("external-link"),
        Icon::Pencil => file!("pencil"),
        Icon::Download => file!("download"),
        Icon::Power => file!("power"),
        Icon::Log => file!("file-text"),
        Icon::Refresh => file!("refresh-cw"),
        Icon::Film => file!("film"),
        Icon::Check => file!("check"),
        Icon::Camera => file!("camera"),
        Icon::Link => file!("link"),
        Icon::Qr => file!("qr-code"),
        Icon::Mic => file!("mic"),
        Icon::Image => file!("image"),
        Icon::Phone => file!("smartphone"),
        Icon::Snail => file!("snail"),
        Icon::Share => file!("share-2"),
        Icon::Globe => file!("globe"),
        Icon::Flag => file!("flag"),
        Icon::Layers => file!("layers"),
        Icon::ChevronDown => file!("chevron-down"),
        Icon::Selected => file!("square-check"),
        Icon::Square => file!("square"),
        Icon::Portrait => file!("portrait"),
        Icon::Music => file!("music"),
        Icon::Sparkles => file!("sparkles"),
        Icon::Record => file!("circle-dot"),
        Icon::Tag => file!("tag"),
        Icon::Undo => file!("undo-2"),
        Icon::Redo => file!("redo-2"),
        Icon::Trophy => file!("trophy"),
        Icon::Flame => file!("flame"),
        Icon::Lock => file!("lock"),
        Icon::Clock => file!("clock"),
    }
}

/// A filled variant: the outline's paths painted solid (for a chosen star, play keys).
fn filled(icon: Icon) -> Vec<u8> {
    String::from_utf8_lossy(source(icon))
        .replace("fill=\"none\"", "fill=\"currentColor\"")
        .into_bytes()
}

pub fn icon<'a>(i: Icon, size: f32, color: Color) -> Svg<'a> {
    svg(svg::Handle::from_memory(source(i)))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_, _| svg::Style { color: Some(color) })
}

pub fn icon_filled<'a>(i: Icon, size: f32, color: Color) -> Svg<'a> {
    svg(svg::Handle::from_memory(filled(i)))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_, _| svg::Style { color: Some(color) })
}
