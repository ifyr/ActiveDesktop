// Reference: heraklessii/Muivly (Apache-2.0), power/apps.rs — per-window foreground/occupancy approach.
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongW, GetWindowRect, IsIconic, IsWindowVisible,
    GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

/// Window classes that never mean "the user is looking at an app on this screen".
fn is_shell_class(class: &str) -> bool {
    matches!(
        class,
        "Progman"
            | "WorkerW"
            | "SHELLDLL_DefView"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "ActiveDesktopWallpaperWindow"
    )
}

/// Decide whether a top-level window counts as occupying a screen.
pub fn is_countable_window(
    class: &str,
    visible: bool,
    iconic: bool,
    cloaked: bool,
    toolwindow: bool,
    rect: (i32, i32, i32, i32),
) -> bool {
    if !visible || iconic || cloaked || toolwindow {
        return false;
    }
    if is_shell_class(class) {
        return false;
    }
    let (left, top, right, bottom) = rect;
    right > left && bottom > top
}

/// One flag per monitor rect: true when a countable top-level window sits on it.
pub fn occupied_monitors(monitor_rects: &[RECT]) -> Vec<bool> {
    struct Ctx<'a> {
        rects: &'a [RECT],
        flags: &'a mut Vec<bool>,
    }

    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let ctx = &mut *(lp.0 as *mut Ctx);
        if let Some(idx) = monitor_index_for_window(hwnd, ctx.rects) {
            // Re-check the per-window filter only for windows we can attribute.
            if window_counts(hwnd) {
                ctx.flags[idx] = true;
            }
        }
        BOOL(1)
    }

    let mut flags = vec![false; monitor_rects.len()];
    if monitor_rects.is_empty() {
        return flags;
    }
    let mut ctx = Ctx {
        rects: monitor_rects,
        flags: &mut flags,
    };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    flags
}

unsafe fn window_counts(hwnd: HWND) -> bool {
    if !IsWindowVisible(hwnd).as_bool() {
        return false;
    }
    if IsIconic(hwnd).as_bool() {
        return false;
    }
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    let toolwindow = (ex & WS_EX_TOOLWINDOW.0) != 0;
    let mut cloaked: u32 = 0;
    let _ = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut _,
        std::mem::size_of::<u32>() as u32,
    );
    let class = class_name(hwnd);
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_err() {
        return false;
    }
    is_countable_window(
        &class,
        true,
        false,
        cloaked != 0,
        toolwindow,
        (rect.left, rect.top, rect.right, rect.bottom),
    )
}

unsafe fn monitor_index_for_window(hwnd: HWND, rects: &[RECT]) -> Option<usize> {
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !GetMonitorInfoW(monitor, &mut info).as_bool() {
        return None;
    }
    rects.iter().position(|r| {
        r.left == info.rcMonitor.left
            && r.top == info.rcMonitor.top
            && r.right == info.rcMonitor.right
            && r.bottom == info.rcMonitor.bottom
    })
}

unsafe fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = GetClassNameW(hwnd, &mut buf);
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

#[cfg(test)]
mod tests {
    use super::is_countable_window;

    #[test]
    fn normal_visible_app_window_counts() {
        assert!(is_countable_window(
            "Notepad",
            true,
            false,
            false,
            false,
            (0, 0, 800, 600)
        ));
    }

    #[test]
    fn minimized_tool_cloaked_or_shell_do_not_count() {
        assert!(!is_countable_window(
            "Notepad",
            true,
            true,
            false,
            false,
            (0, 0, 800, 600)
        ));
        assert!(!is_countable_window(
            "Notepad",
            true,
            false,
            false,
            true,
            (0, 0, 800, 600)
        ));
        assert!(!is_countable_window(
            "Notepad",
            true,
            false,
            true,
            false,
            (0, 0, 800, 600)
        ));
        assert!(!is_countable_window(
            "Notepad",
            false,
            false,
            false,
            false,
            (0, 0, 800, 600)
        ));
        assert!(!is_countable_window(
            "Progman",
            true,
            false,
            false,
            false,
            (0, 0, 1920, 1080)
        ));
        assert!(!is_countable_window(
            "Shell_TrayWnd",
            true,
            false,
            false,
            false,
            (0, 1040, 1920, 1080)
        ));
        assert!(!is_countable_window(
            "ActiveDesktopWallpaperWindow",
            true,
            false,
            false,
            false,
            (0, 0, 1920, 1080)
        ));
    }

    #[test]
    fn empty_rect_does_not_count() {
        assert!(!is_countable_window(
            "Notepad",
            true,
            false,
            false,
            false,
            (0, 0, 0, 0)
        ));
    }
}
