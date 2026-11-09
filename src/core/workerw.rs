// References (concepts/algorithms reimplemented; no verbatim code copied):
// - Shogun65/Worker-Finder (MIT): 0x052C argument combinations and WorkerW lookup.
// - heraklessii/Muivly (Apache-2.0): dual child/sibling layout, desktop-size validation, liveness check.
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, GetWindowRect, IsWindow, IsWindowVisible,
    SendMessageTimeoutW, SMTO_NORMAL,
};

pub fn can_cover(w: i32, h: i32) -> bool {
    w >= 640 && h >= 480
}

const WM_SPAWN_WORKER: u32 = 0x052C;

pub fn find() -> Option<HWND> {
    unsafe {
        let progman = FindWindowW(windows::core::w!("Progman"), PCWSTR::null()).ok()?;
        for (wp, lp) in [(0xD, 0x1), (0, 0)] {
            let _ = SendMessageTimeoutW(
                progman,
                WM_SPAWN_WORKER,
                WPARAM(wp),
                LPARAM(lp),
                SMTO_NORMAL,
                1000,
                None,
            );
        }
        if let Ok(child) = FindWindowExW(
            progman,
            HWND(std::ptr::null_mut()),
            windows::core::w!("WorkerW"),
            PCWSTR::null(),
        ) {
            if covers(child) {
                return Some(child);
            }
        }
        let mut found = HWND(std::ptr::null_mut());
        unsafe extern "system" fn cb(top: HWND, lp: LPARAM) -> BOOL {
            let out = &mut *(lp.0 as *mut HWND);
            if FindWindowExW(
                top,
                HWND(std::ptr::null_mut()),
                windows::core::w!("SHELLDLL_DefView"),
                PCWSTR::null(),
            )
            .is_ok()
            {
                if let Ok(w) = FindWindowExW(
                    HWND(std::ptr::null_mut()),
                    top,
                    windows::core::w!("WorkerW"),
                    PCWSTR::null(),
                ) {
                    *out = w;
                    return BOOL(0);
                }
            }
            BOOL(1)
        }
        let _ = EnumWindows(Some(cb), LPARAM(&mut found as *mut HWND as isize));
        if !found.0.is_null() && covers(found) {
            return Some(found);
        }
        Some(progman)
    }
}

fn covers(h: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(h).as_bool() {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(h, &mut r).is_err() {
            return false;
        }
        can_cover(r.right - r.left, r.bottom - r.top)
    }
}

pub fn is_alive(h: HWND) -> bool {
    unsafe { IsWindow(h).as_bool() }
}
