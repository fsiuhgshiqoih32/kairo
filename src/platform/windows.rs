use std::process::Command;
use std::ptr::null_mut;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, MB_ICONERROR, MB_OK, MONITORINFOF_PRIMARY, MessageBoxW,
};

use rewindr::args::Source;

use super::Monitor;

pub fn init_process() {
    unsafe {
        // Physical pixels everywhere, so monitor sizes match what capture delivers.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // Release builds have no console; reattach to the launching terminal so CLI output shows.
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

pub fn show_error(message: &str) {
    let text = wide(message);
    let title = wide("Rewindr");
    unsafe {
        MessageBoxW(
            null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn collect(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> i32 {
        let list = unsafe { &mut *(data as *mut Vec<HMONITOR>) };
        list.push(m);
        1
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            std::ptr::null(),
            Some(collect),
            &mut handles as *mut _ as LPARAM,
        )
    };
    let outputs = dxgi_outputs();
    let mut list: Vec<Monitor> = handles
        .into_iter()
        .filter_map(|h| {
            let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if unsafe { GetMonitorInfoW(h, &mut info as *mut _ as *mut MONITORINFO) } == 0 {
                return None;
            }
            let r = info.monitorInfo.rcMonitor;
            let device = from_wide(&info.szDevice);
            let number = device
                .trim_start_matches("\\\\.\\DISPLAY")
                .parse::<u32>()
                .ok();
            Some(Monitor {
                name: number.map_or_else(|| device.clone(), |n| format!("Display {n}")),
                x: r.left,
                y: r.top,
                w: (r.right - r.left).max(0) as u32,
                h: (r.bottom - r.top).max(0) as u32,
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                handle: h as u64,
                system_index: outputs
                    .iter()
                    .position(|d| d.eq_ignore_ascii_case(&device))
                    .map(|i| i as u32),
            })
        })
        .collect();
    list.sort_by_key(|m| (m.x, m.y));
    list
}

/// Device names of the outputs of the default GPU, in DXGI order: Desktop Duplication
/// (`ddagrab`) numbers outputs this way.
fn dxgi_outputs() -> Vec<String> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    let mut out = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return out;
        };
        let Ok(adapter) = factory.EnumAdapters1(0) else {
            return out;
        };
        let mut i = 0;
        while let Ok(output) = adapter.EnumOutputs(i) {
            if let Ok(desc) = output.GetDesc() {
                out.push(from_wide(&desc.DeviceName));
            }
            i += 1;
        }
    }
    out
}

/// Ways to capture `m`, best first: Windows Graphics Capture (GPU, scales on the GPU), Desktop
/// Duplication (GPU), GDI (CPU, works almost anywhere).
pub fn capture_sources(m: &Monitor) -> Vec<Source> {
    let mut v = vec![Source::WindowsGraphics { hmonitor: m.handle }];
    if let Some(i) = m.system_index {
        v.push(Source::Ddagrab { output: i });
    }
    v.push(Source::Gdigrab {
        x: m.x,
        y: m.y,
        w: m.w,
        h: m.h,
    });
    v
}

pub fn foreground_title() -> Option<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
    }
}

pub fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

pub fn play_saved_sound() {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    let wav = WAV.get_or_init(super::saved_sound_wav);
    unsafe {
        PlaySoundW(
            wav.as_ptr() as *const u16,
            null_mut(),
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        )
    };
}

/// Puts files on the clipboard the way Explorer's Copy does, so they paste into Discord, chats
/// and folders.
pub fn copy_files(paths: &[std::path::PathBuf]) -> Result<(), String> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };
    use windows_sys::Win32::UI::Shell::DROPFILES;
    const CF_HDROP: u32 = 15;
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        let abs = std::fs::canonicalize(p).map_err(|e| e.to_string())?;
        let s = abs
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_string();
        list.extend(s.encode_utf16());
        list.push(0);
    }
    list.push(0);
    let header = size_of::<DROPFILES>();
    let total = header + list.len() * 2;
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, total);
        if mem.is_null() {
            return Err("out of memory".into());
        }
        let ptr = GlobalLock(mem) as *mut u8;
        let df = ptr as *mut DROPFILES;
        (*df).pFiles = header as u32;
        (*df).fWide = 1;
        (*df).pt.x = 0;
        (*df).pt.y = 0;
        (*df).fNC = 0;
        std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, ptr.add(header), list.len() * 2);
        GlobalUnlock(mem);
        if OpenClipboard(null_mut()) == 0 {
            return Err("the clipboard is busy".into());
        }
        EmptyClipboard();
        let ok = !SetClipboardData(CF_HDROP, mem).is_null();
        CloseClipboard();
        if ok {
            Ok(())
        } else {
            Err("could not set the clipboard".into())
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// The primary monitor's area without the taskbar: x, y, width, height.
pub fn work_area() -> Option<(i32, i32, u32, u32)> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW};
    let mut r: RECT = unsafe { std::mem::zeroed() };
    let ok =
        unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut r as *mut RECT as *mut _, 0) } != 0;
    ok.then(|| {
        (
            r.left,
            r.top,
            (r.right - r.left).max(0) as u32,
            (r.bottom - r.top).max(0) as u32,
        )
    })
}

/// Shows a hidden window without activating it, so a game keeps the focus and the keyboard.
/// Clicks on it don't activate it either.
pub fn show_without_focus(raw: u64) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW,
    };
    let hwnd = raw as usize as windows_sys::Win32::Foundation::HWND;
    if hwnd.is_null() {
        return false;
    }
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            ex | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) as isize,
        );
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
    true
}
