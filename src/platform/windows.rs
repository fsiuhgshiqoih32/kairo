use std::process::Command;
use std::ptr::null_mut;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MONITORINFOEXW, MonitorFromWindow,
};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
    MB_ICONERROR, MB_OK, MONITORINFOF_PRIMARY, MessageBoxW,
};

use kairo::args::Source;

use super::Monitor;

/// Windows' "Choose your app mode": `Some(true)` for light, `None` if it can't be read.
/// Windows' "Animation effects" is off (Settings, Accessibility, Visual effects).
pub fn animations_off() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };
    let mut on: i32 = 1;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL into `on`.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut on as *mut i32).cast(),
            0,
        )
    };
    ok != 0 && on == 0
}

pub fn prefers_light() -> Option<bool> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let wide = |s: &str| {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = wide("AppsUseLightTheme");
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the strings are NUL-terminated and `value` holds the 4 bytes asked for.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        )
    };
    (r == 0).then_some(value != 0)
}

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
    let title = wide("Kairo");
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
    let mut v = vec![Source::WindowsGraphics {
        hmonitor: m.handle,
        sdr_white: hdr_sdr_white(m.handle),
    }];
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

/// The SDR white level in nits ("SDR content brightness") of the screen `hmonitor` while it's in
/// HDR mode; `None` in SDR, or when Windows won't say.
pub fn hdr_sdr_white(hmonitor: u64) -> Option<u32> {
    use windows_sys::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
        QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };
    // The screen's GDI name ("\\.\DISPLAY1"), which the display paths are found by.
    let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` is a MONITORINFOEXW with its size set.
    if unsafe {
        GetMonitorInfoW(
            hmonitor as HMONITOR,
            &mut info as *mut _ as *mut MONITORINFO,
        )
    } == 0
    {
        return None;
    }
    let wide_str = |w: &[u16]| {
        let n = w.iter().position(|c| *c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..n])
    };
    let name = wide_str(&info.szDevice);
    let (mut np, mut nm) = (0u32, 0u32);
    // SAFETY: two counts out.
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) } != 0 {
        return None;
    }
    let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![unsafe { std::mem::zeroed() }; np as usize];
    let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![unsafe { std::mem::zeroed() }; nm as usize];
    // SAFETY: the arrays hold the counts given.
    let r = unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut np,
            paths.as_mut_ptr(),
            &mut nm,
            modes.as_mut_ptr(),
            null_mut(),
        )
    };
    if r != 0 {
        return None;
    }
    let header = |kind, size: usize, adapter, id| DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size as u32,
        adapterId: adapter,
        id,
    };
    for p in &paths[..np as usize] {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
                p.sourceInfo.adapterId,
                p.sourceInfo.id,
            ),
            viewGdiDeviceName: [0; 32],
        };
        // SAFETY: the header gives the struct's kind and size.
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0
            || wide_str(&source.viewGdiDeviceName) != name
        {
            continue;
        }
        let mut color: DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO = unsafe { std::mem::zeroed() };
        color.header = header(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(),
            p.targetInfo.adapterId,
            p.targetInfo.id,
        );
        // SAFETY: as above.
        if unsafe { DisplayConfigGetDeviceInfo(&mut color.header) } != 0 {
            return None;
        }
        // Bit 1: advanced colour (HDR) is on.
        // SAFETY: a plain u32 view of the flags.
        if unsafe { color.Anonymous.value } & 0x2 == 0 {
            return None;
        }
        let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(),
                p.targetInfo.adapterId,
                p.targetInfo.id,
            ),
            SDRWhiteLevel: 0,
        };
        // SAFETY: as above.
        if unsafe { DisplayConfigGetDeviceInfo(&mut white.header) } != 0 {
            return None;
        }
        // In thousandths of 80 nits.
        let nits = white.SDRWhiteLevel as u64 * 80 / 1000;
        return Some(nits.clamp(80, 2000) as u32);
    }
    None
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

/// Programs that go full screen without being games: browsers, video players, the shell.
const NOT_GAMES: [&str; 14] = [
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "opera.exe",
    "brave.exe",
    "vivaldi.exe",
    "vlc.exe",
    "mpc-hc64.exe",
    "mpc-be64.exe",
    "potplayermini64.exe",
    "explorer.exe",
    "applicationframehost.exe",
    "powerpnt.exe",
    "kairo.exe",
];

/// Whether a game seems to be in front: a window that covers its whole screen (full screen or
/// borderless), and isn't a browser, video player or the desktop. `None` if it can't tell.
pub fn game_in_front() -> Option<bool> {
    // SAFETY: plain Win32 queries on the foreground window; every buffer is sized as passed.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return Some(false);
        }
        let mut class = [0u16; 64];
        let n = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
        let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
        if [
            "Progman",
            "WorkerW",
            "Shell_TrayWnd",
            "Shell_SecondaryTrayWnd",
        ]
        .contains(&class.as_str())
        {
            return Some(false);
        }
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut r) == 0 {
            return None;
        }
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut mi) == 0 {
            return None;
        }
        let m = mi.rcMonitor;
        if r.left > m.left || r.top > m.top || r.right < m.right || r.bottom < m.bottom {
            return Some(false);
        }
        let exe = window_exe(hwnd).unwrap_or_default().to_lowercase();
        Some(!NOT_GAMES.iter().any(|n| exe.ends_with(n)))
    }
}

/// The program file behind a window.
unsafe fn window_exe(hwnd: windows_sys::Win32::Foundation::HWND) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    let mut pid = 0u32;
    // SAFETY: the caller passes a live window handle; the process handle is closed below.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        (ok != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

pub fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Like `hide_console`, and below normal priority: a save or an export gives way to the game.
pub fn hide_console_low_priority(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
    cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
}

pub fn play_marker_sound() {
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    play_wav(WAV.get_or_init(super::marker_sound_wav));
}

/// A built-in save sound, made once and kept for the life of the process.
pub fn play_builtin(kind: &'static str) {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static MADE: OnceLock<Mutex<HashMap<&'static str, &'static [u8]>>> = OnceLock::new();
    let wav = *MADE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .entry(kind)
        .or_insert_with(|| Box::leak(super::save_sound_wav(kind).into_boxed_slice()));
    play_wav(wav);
}

/// Plays a WAV file.
pub fn play_sound_file(path: &std::path::Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: SND_FILENAME copies the NUL-terminated path before returning.
    unsafe {
        PlaySoundW(
            wide.as_ptr(),
            null_mut(),
            SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
        )
    };
}

fn play_wav(wav: &'static [u8]) {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
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
/// Clicks on it don't activate it either. On Windows 11 its corners are rounded, so no square
/// corners show around the pop-up's rounded card (Windows 10 ignores this).
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
        use windows_sys::Win32::Graphics::Dwm::{
            DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
        };
        let round = DWMWCP_ROUND;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &round as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&round) as u32,
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

/// Bytes free for this user on the drive `dir` is on (`dir` must exist).
pub fn free_space(dir: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = dir
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free = 0u64;
    // SAFETY: a NUL-terminated path, and a u64 to write to; the totals aren't asked for.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, null_mut(), null_mut()) };
    (ok != 0).then_some(free)
}

#[cfg(test)]
mod tests {
    /// Each screen and, in HDR mode, its SDR white level. Reads the display settings only.
    #[test]
    #[ignore]
    fn screens_say_whether_they_show_hdr() {
        for m in super::monitors() {
            println!(
                "{} {}x{}: SDR white {:?}",
                m.name,
                m.w,
                m.h,
                super::hdr_sdr_white(m.handle)
            );
        }
    }
}
