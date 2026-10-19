//! Keys and clicks for the overlay (Windows): low-level keyboard and mouse hooks on a thread of
//! their own. Only the keys the overlay shows are kept, and only while Rewindr records; nothing
//! else typed is seen past the hook.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_UP, MSG,
    PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_QUIT,
    WM_RBUTTONDOWN, WM_RBUTTONUP,
};

use crate::recorder::{Shared, now_us};

/// Where the hooks put what they see (hook procedures can't carry state).
static SINK: Mutex<Option<Arc<Shared>>> = Mutex::new(None);
/// Keys down now, so held keys' repeats aren't kept.
static DOWN: AtomicU32 = AtomicU32::new(0);

/// The overlay's id for a virtual-key code, if it shows that key.
fn key_id(vk: u32) -> Option<u8> {
    Some(match vk {
        0x51 => 0,                             // Q
        0x57 => 1,                             // W
        0x45 => 2,                             // E
        0x52 => 3,                             // R
        0x41 => 4,                             // A
        0x53 => 5,                             // S
        0x44 => 6,                             // D
        0x46 => 7,                             // F
        0x10 | 0xA0 | 0xA1 => 8,               // Shift
        0x11 | 0xA2 | 0xA3 => 9,               // Ctrl
        0x20 => 10,                            // Space
        0x09 => 11,                            // Tab
        0x31..=0x34 => (vk - 0x31 + 12) as u8, // 1 to 4
        _ => return None,
    })
}

fn record(key: u8, down: bool) {
    let was = DOWN.load(Ordering::Relaxed) & (1 << key) != 0;
    if was == down {
        return;
    }
    if down {
        DOWN.fetch_or(1 << key, Ordering::Relaxed);
    } else {
        DOWN.fetch_and(!(1 << key), Ordering::Relaxed);
    }
    if let Some(shared) = SINK.lock().unwrap().as_ref() {
        shared.input(now_us(), key, down);
    }
}

unsafe extern "system" fn keyboard(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_KEYBOARD_LL, lParam points at a KBDLLHOOKSTRUCT.
        let k = unsafe { &*(l as *const KBDLLHOOKSTRUCT) };
        if let Some(id) = key_id(k.vkCode) {
            record(id, k.flags & LLKHF_UP == 0);
        }
    }
    // SAFETY: passing the event on, as every hook must.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
}

unsafe extern "system" fn mouse(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0 {
        use rewindr::keys::{MOUSE_LEFT, MOUSE_MIDDLE, MOUSE_RIGHT};
        match w as u32 {
            WM_LBUTTONDOWN => record(MOUSE_LEFT, true),
            WM_LBUTTONUP => record(MOUSE_LEFT, false),
            WM_RBUTTONDOWN => record(MOUSE_RIGHT, true),
            WM_RBUTTONUP => record(MOUSE_RIGHT, false),
            WM_MBUTTONDOWN => record(MOUSE_MIDDLE, true),
            WM_MBUTTONUP => record(MOUSE_MIDDLE, false),
            _ => {}
        }
    }
    // SAFETY: passing the event on.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
}

/// The hooks; dropping it removes them.
pub struct Hooks {
    thread_id: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Hooks {
    fn drop(&mut self) {
        let id = self.thread_id.load(Ordering::SeqCst);
        if id != 0 {
            // SAFETY: asks the hook thread's message loop to end.
            unsafe { PostThreadMessageW(id, WM_QUIT, 0, 0) };
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        *SINK.lock().unwrap() = None;
        DOWN.store(0, Ordering::Relaxed);
    }
}

/// Starts watching keys and clicks into `shared`.
pub fn start(shared: Arc<Shared>) -> Result<Hooks, String> {
    *SINK.lock().unwrap() = Some(shared);
    let thread_id = Arc::new(AtomicU32::new(0));
    let (tx, rx) = std::sync::mpsc::channel();
    let thread = {
        let thread_id = thread_id.clone();
        std::thread::Builder::new()
            .name("input".into())
            .spawn(move || {
                // SAFETY: hooks installed and removed on this thread, which pumps messages for
                // them until WM_QUIT.
                unsafe {
                    thread_id.store(GetCurrentThreadId(), Ordering::SeqCst);
                    let module = GetModuleHandleW(std::ptr::null());
                    let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0);
                    let ms = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), module, 0);
                    let _ = tx.send(!kb.is_null() && !ms.is_null());
                    let mut msg: MSG = std::mem::zeroed();
                    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    if !kb.is_null() {
                        UnhookWindowsHookEx(kb);
                    }
                    if !ms.is_null() {
                        UnhookWindowsHookEx(ms);
                    }
                }
            })
            .map_err(|e| e.to_string())?
    };
    let hooks = Hooks {
        thread_id,
        thread: Some(thread),
    };
    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(true) => Ok(hooks),
        _ => Err("couldn't watch the keyboard and mouse".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_overlay_keys_are_kept() {
        assert_eq!(key_id(0x57), Some(1));
        assert_eq!(key_id(0xA1), Some(8));
        assert_eq!(key_id(0x34), Some(15));
        // Letters the overlay doesn't show, digits past 4, Enter: not seen at all.
        for vk in [0x48, 0x49, 0x4C, 0x35, 0x39, 0x0D] {
            assert_eq!(key_id(vk), None);
        }
        assert_eq!(rewindr::keys::KEYS[key_id(0x20).unwrap() as usize], "Space");
    }
}
