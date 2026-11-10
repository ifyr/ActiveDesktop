#![allow(clippy::items_after_test_module)]
// Reference: wangwenx190/dynamic-desktop (GPL-3.0) — concept only (parenting a window under WorkerW); no GPL code copied.

use anyhow::{anyhow, bail, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClassNameW, GetParent,
    GetWindowRect, IsWindowVisible, PeekMessageW, RegisterClassW, SetParent, SetWindowPos,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, HWND_BOTTOM, MSG, PM_REMOVE, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, WINDOW_STYLE, WM_QUIT, WNDCLASSW, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_VISIBLE,
};

use super::decoder::{self, Decoder};
use super::renderer::{self, Renderer};
use super::{config, occupancy, workerw};

const WINDOW_CLASS: PCWSTR = w!("ActiveDesktopWallpaperWindow");
const FRAME_INTERVAL: Duration = Duration::from_millis(1000 / 30);
const OCCUPANCY_INTERVAL: Duration = Duration::from_millis(250);
const VIDEO_CHECK_TICKS: u64 = 60;
const VIDEO_SETTLE: Duration = Duration::from_secs(1);
const WM_DISPLAYCHANGE: u32 = 0x007E;
const REBUILD_CHECK_TICKS: u64 = 30;
const REBUILD_MAX_ATTEMPTS: u32 = 10;
const REBUILD_RETRY_DELAY: Duration = Duration::from_millis(1000);
/// After the fast retries are exhausted the engine stays alive and keeps
/// attempting at this low frequency forever instead of exiting as a zombie.
const REBUILD_SLOW_RETRY_DELAY: Duration = Duration::from_secs(2);

static DISPLAY_CHANGE: AtomicBool = AtomicBool::new(false);

#[derive(Debug, PartialEq, Eq)]
enum RebuildReason {
    WorkerLost,
    DisplayChanged,
}

fn check_rebuild(worker_alive: bool, display_changed: bool) -> Option<RebuildReason> {
    if !worker_alive {
        Some(RebuildReason::WorkerLost)
    } else if display_changed {
        Some(RebuildReason::DisplayChanged)
    } else {
        None
    }
}

fn monitor_signature(monitors: &[MONITORINFO]) -> Vec<(i32, i32, i32, i32)> {
    monitors
        .iter()
        .map(|m| {
            (
                m.rcMonitor.left,
                m.rcMonitor.top,
                m.rcMonitor.right,
                m.rcMonitor.bottom,
            )
        })
        .collect()
}

fn next_frame_deadline(now: Instant, next: Instant) -> Instant {
    let candidate = next + FRAME_INTERVAL;
    if candidate > now {
        candidate
    } else {
        now
    }
}

fn should_draw_surface(paused: bool, force: bool, occupied: bool) -> bool {
    !paused && (force || !occupied)
}

fn desired_video(video_override: Option<&str>, i: usize) -> String {
    match video_override {
        Some(v) => v.to_string(),
        None => config::resolve_video_for(i + 1, &config::exe_dir_or_cwd())
            .to_string_lossy()
            .into_owned(),
    }
}

fn file_stamp(path: &str) -> (u64, u64) {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (meta.len(), mtime)
        }
        Err(_) => (0, 0),
    }
}

fn video_stamps_for(groups: &[VideoGroup]) -> Vec<(String, u64, u64)> {
    groups
        .iter()
        .map(|g| {
            let (len, mtime) = file_stamp(&g.video);
            (g.video.clone(), len, mtime)
        })
        .collect()
}

fn force_draw_enabled() -> bool {
    std::env::var("ACTIVEDESKTOP_FORCE_DRAW")
        .map(|v| v == "1")
        .unwrap_or(false)
}

pub struct EngineHandle {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl EngineHandle {
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    pub fn stop(&mut self) {
        self.shutdown();
    }

    /// True once the engine thread has ended, so `main` can detect a dead
    /// engine instead of idling forever as a silent zombie.
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // The decoders lived on the engine thread and are gone once it joined;
        // pair the process-wide MFStartup now so a graceful exit is clean.
        decoder::shutdown_mf();
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn spawn_wallpaper(video_override: Option<&str>) -> Result<EngineHandle> {
    log::info!("engine: spawn_wallpaper override={:?}", video_override);
    let (tx, rx) = mpsc::channel::<Result<()>>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = stop.clone();
    let paused = Arc::new(AtomicBool::new(false));
    let paused_for_thread = paused.clone();
    let video_override = video_override.map(|v| v.to_string());
    let thread = std::thread::Builder::new()
        .name("wallpaper-engine".into())
        .spawn(move || engine_main(video_override, stop_for_thread, paused_for_thread, tx))?;

    match rx.recv() {
        Ok(Ok(())) => Ok(EngineHandle {
            stop,
            paused,
            thread: Some(thread),
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            decoder::shutdown_mf();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            decoder::shutdown_mf();
            Err(anyhow!("engine thread died before setup completed"))
        }
    }
}

fn engine_main(
    video_override: Option<String>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    tx: mpsc::Sender<Result<()>>,
) {
    match unsafe { Session::setup(video_override.as_deref()) } {
        Ok(mut session) => {
            let _ = tx.send(Ok(()));
            session.run(&stop, &paused);
            session.teardown();
            log::info!("engine: render loop stopped");
        }
        Err(e) => {
            log::error!("engine: setup failed: {e:?}");
            let _ = tx.send(Err(e));
        }
    }
}

struct VideoGroup {
    video: String,
    decoder: Decoder,
}

struct MonitorSurface {
    index: usize,
    hwnd: HWND,
    width: u32,
    height: u32,
    rect: RECT,
    occupied: bool,
    group: usize,
    renderer: Renderer,
    frames: u64,
    errors: u64,
}

struct Session {
    target: HWND,
    monitors: Vec<MonitorSurface>,
    groups: Vec<VideoGroup>,
    video_stamps: Vec<(String, u64, u64)>,
    video_override: Option<String>,
    display_sig: Vec<(i32, i32, i32, i32)>,
    notify: Option<HWND>,
}

impl Session {
    unsafe fn setup(video_override: Option<&str>) -> Result<Self> {
        let target = workerw::find().ok_or_else(|| anyhow!("neither WorkerW nor Progman found"))?;
        let target_class = window_class(target);
        let target_rect = window_rect(target);
        log::info!(
            "engine: target hwnd={:?} class={} rect={}",
            target,
            target_class,
            fmt_rect(target_rect)
        );
        if target_class == "Progman" {
            log::warn!(
                "engine: target is Progman fallback (not a real WorkerW); wallpaper may sit above icons"
            );
        }

        let monitors = enum_monitors()?;
        let display_sig = monitor_signature(&monitors);
        log::info!("engine: monitors detected = {}", monitors.len());
        for (i, m) in monitors.iter().enumerate() {
            log::info!("engine: monitor[{}] rcMonitor={}", i, fmt_rect(m.rcMonitor));
        }

        let module = GetModuleHandleW(None)?;
        let instance = HINSTANCE(module.0);
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: Default::default(),
            hCursor: Default::default(),
            hbrBackground: Default::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: WINDOW_CLASS,
        };
        if RegisterClassW(&wc) == 0 {
            log::warn!(
                "engine: RegisterClassW returned 0 (class may already be registered): {}",
                windows::core::Error::from_win32()
            );
        }

        let notify = match CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            WINDOW_CLASS,
            w!("ActiveDesktop Notify"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND(std::ptr::null_mut()),
            None,
            instance,
            None,
        ) {
            Ok(hwnd) => Some(hwnd),
            Err(e) => {
                log::warn!("engine: notify window creation failed: {e:?}");
                None
            }
        };

        let (device, ctx) = Renderer::create_device()?;
        let parent_rect = window_rect(target);
        let mut surfaces = Vec::new();
        let mut groups: Vec<VideoGroup> = Vec::new();
        for (i, m) in monitors.iter().enumerate() {
            let rc = m.rcMonitor;
            let w = rc.right - rc.left;
            let h = rc.bottom - rc.top;
            if w <= 0 || h <= 0 {
                log::warn!("engine: monitor[{}] has empty rect, skipping", i);
                continue;
            }
            let px = rc.left - parent_rect.left;
            let py = rc.top - parent_rect.top;
            let hwnd = match CreateWindowExW(
                WS_EX_NOACTIVATE,
                WINDOW_CLASS,
                w!("ActiveDesktop Wallpaper"),
                WS_CHILD | WS_VISIBLE,
                px,
                py,
                w,
                h,
                target,
                None,
                instance,
                None,
            ) {
                Ok(hwnd) => hwnd,
                Err(e) => {
                    log::error!("engine: monitor[{}] CreateWindowExW failed: {e:?}", i);
                    continue;
                }
            };

            match SetParent(hwnd, target) {
                Ok(old) => log::info!("engine: monitor[{}] SetParent ok, old parent={:?}", i, old),
                Err(e) => log::warn!(
                    "engine: monitor[{}] SetParent failed: {e:?}; verifying with GetParent",
                    i
                ),
            }

            let parent = GetParent(hwnd).ok();
            if parent != Some(target) {
                log::error!(
                    "engine: monitor[{}] attach failed, GetParent={:?} expected={:?}; skipping",
                    i,
                    parent,
                    target
                );
                let _ = DestroyWindow(hwnd);
                continue;
            }

            if let Err(e) = SetWindowPos(
                hwnd,
                HWND_BOTTOM,
                px,
                py,
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            ) {
                log::warn!("engine: monitor[{}] SetWindowPos failed: {e:?}", i);
            }

            let parent_class = window_class(parent.unwrap());
            let actual = window_rect(hwnd);
            let visible = IsWindowVisible(hwnd).as_bool();
            log::info!(
                "engine: monitor[{}] hwnd={:?} parent={:?} parent_class={} requested={} actual={} visible={}",
                i,
                hwnd,
                parent.unwrap(),
                parent_class,
                fmt_rect(rc),
                fmt_rect(actual),
                visible
            );

            let renderer = match Renderer::new_with_device(hwnd, w as u32, h as u32, &device, &ctx)
            {
                Ok(renderer) => renderer,
                Err(e) => {
                    log::error!(
                        "engine: monitor[{}] Renderer::new failed: {e:?}; skipping",
                        i
                    );
                    let _ = DestroyWindow(hwnd);
                    continue;
                }
            };
            let video = desired_video(video_override, i);
            log::info!("engine: monitor[{}] video={:?}", i, video);
            let group = match groups.iter().position(|g| g.video == video) {
                Some(group) => group,
                None => match Decoder::open(&video, &device) {
                    Ok(decoder) => {
                        groups.push(VideoGroup {
                            video: video.clone(),
                            decoder,
                        });
                        groups.len() - 1
                    }
                    Err(e) => {
                        log::error!(
                            "engine: monitor[{}] Decoder::open({:?}) failed: {e:?}; skipping",
                            i,
                            video
                        );
                        let _ = DestroyWindow(hwnd);
                        continue;
                    }
                },
            };

            surfaces.push(MonitorSurface {
                index: i,
                hwnd,
                width: w as u32,
                height: h as u32,
                rect: rc,
                occupied: false,
                group,
                renderer,
                frames: 0,
                errors: 0,
            });
        }

        if surfaces.is_empty() {
            if let Some(h) = notify {
                let _ = DestroyWindow(h);
            }
            bail!("no monitor windows could be created");
        }
        log::info!("engine: {} wallpaper window(s) attached", surfaces.len());
        log::info!("engine: {} shared decoder group(s)", groups.len());
        DISPLAY_CHANGE.store(false, Ordering::SeqCst);
        Ok(Self {
            target,
            monitors: surfaces,
            video_stamps: video_stamps_for(&groups),
            groups,
            video_override: video_override.map(|v| v.to_string()),
            display_sig,
            notify,
        })
    }

    fn teardown(&mut self) {
        // Drop decoders before the windows and device they were built on.
        self.groups.clear();
        let old = std::mem::take(&mut self.monitors);
        let hwnds: Vec<HWND> = old.iter().map(|m| m.hwnd).collect();
        drop(old);
        unsafe {
            for hwnd in hwnds {
                let _ = DestroyWindow(hwnd);
            }
            if let Some(h) = self.notify.take() {
                let _ = DestroyWindow(h);
            }
        }
    }

    /// Recreate the wallpaper session once. Used by the fast retry loop and by
    /// the engine's low-frequency background retry after exhaustion.
    fn try_rebuild_once(&mut self) -> Result<()> {
        let video_override = self.video_override.clone();
        let session = unsafe { Self::setup(video_override.as_deref()) }?;
        *self = session;
        DISPLAY_CHANGE.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn rebuild(&mut self) -> Result<()> {
        log::warn!(
            "engine: rebuilding wallpaper surfaces (override={:?})",
            self.video_override
        );
        self.teardown();
        let mut last: Option<anyhow::Error> = None;
        for attempt in 1..=REBUILD_MAX_ATTEMPTS {
            match self.try_rebuild_once() {
                Ok(()) => {
                    log::info!("engine: rebuild succeeded on attempt {attempt}");
                    return Ok(());
                }
                Err(e) => {
                    log::warn!(
                        "engine: rebuild attempt {attempt}/{REBUILD_MAX_ATTEMPTS} failed: {e:?}"
                    );
                    last = Some(e);
                    if attempt < REBUILD_MAX_ATTEMPTS {
                        std::thread::sleep(REBUILD_RETRY_DELAY);
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("rebuild failed without a recorded error")))
    }

    fn videos_need_reload(&self) -> bool {
        for (i, surface) in self.monitors.iter().enumerate() {
            if desired_video(self.video_override.as_deref(), i) != self.groups[surface.group].video
            {
                return true;
            }
        }
        let stamps = video_stamps_for(&self.groups);
        if stamps == self.video_stamps {
            return false;
        }
        let now = std::time::SystemTime::now();
        for (path, _, _) in &stamps {
            if let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) {
                if now
                    .duration_since(modified)
                    .map(|age| age < VIDEO_SETTLE)
                    .unwrap_or(false)
                {
                    return false;
                }
            }
        }
        true
    }

    fn monitors_changed(&self) -> bool {
        match unsafe { enum_monitors() } {
            Ok(monitors) => monitor_signature(&monitors) != self.display_sig,
            Err(e) => {
                log::warn!("engine: monitor re-enumeration failed: {e:?}");
                false
            }
        }
    }

    fn run(&mut self, stop: &AtomicBool, paused: &AtomicBool) {
        let mut msg = MSG::default();
        let mut tick: u64 = 0;
        let mut drawn: u64 = 0;
        let mut skipped: u64 = 0;
        let mut next = Instant::now();
        let mut pending_rebuild = false;
        let mut next_rebuild_attempt = Instant::now();
        let force = force_draw_enabled();
        if force {
            log::warn!("engine: ACTIVEDESKTOP_FORCE_DRAW=1; freeze check bypassed");
        }
        let mut occupied: Vec<bool> = vec![false; self.monitors.len()];
        let mut last_occupancy = Instant::now() - OCCUPANCY_INTERVAL;
        while !stop.load(Ordering::SeqCst) {
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            if pending_rebuild {
                // Wallpaper surfaces are down. Keep pumping messages above and
                // retry at a bounded low frequency forever; never exit here, or
                // the process becomes a zombie host with nothing rendered.
                if Instant::now() >= next_rebuild_attempt {
                    match self.try_rebuild_once() {
                        Ok(()) => {
                            log::info!("engine: background rebuild succeeded");
                            pending_rebuild = false;
                            next = Instant::now();
                        }
                        Err(e) => {
                            log::warn!("engine: background rebuild retry failed: {e:?}");
                            next_rebuild_attempt = Instant::now() + REBUILD_SLOW_RETRY_DELAY;
                        }
                    }
                }
            } else {
                let is_paused = paused.load(Ordering::SeqCst);
                if Instant::now().duration_since(last_occupancy) >= OCCUPANCY_INTERVAL {
                    let rects: Vec<RECT> = self.monitors.iter().map(|s| s.rect).collect();
                    occupied = occupancy::occupied_monitors(&rects);
                    for (i, surface) in self.monitors.iter_mut().enumerate() {
                        surface.occupied = occupied.get(i).copied().unwrap_or(false);
                    }
                    last_occupancy = Instant::now();
                }
                if !is_paused {
                    let mut device_lost = false;
                    let mut drew_any = false;
                    let drawable: Vec<bool> = self
                        .monitors
                        .iter()
                        .enumerate()
                        .map(|(i, _)| {
                            should_draw_surface(
                                is_paused,
                                force,
                                occupied.get(i).copied().unwrap_or(false),
                            )
                        })
                        .collect();
                    for group in 0..self.groups.len() {
                        let indices: Vec<usize> = self
                            .monitors
                            .iter()
                            .enumerate()
                            .filter(|(i, surface)| surface.group == group && drawable[*i])
                            .map(|(i, _)| i)
                            .collect();
                        if indices.is_empty() {
                            continue;
                        }
                        match self.groups[group].decoder.next_frame() {
                            Ok(frame) => {
                                for idx in indices {
                                    let surface = &mut self.monitors[idx];
                                    match surface.renderer.draw_nv12(
                                        &frame,
                                        surface.width,
                                        surface.height,
                                    ) {
                                        Ok(()) => match surface.renderer.present() {
                                            Ok(()) => {
                                                surface.frames += 1;
                                                drew_any = true;
                                            }
                                            Err(e) => {
                                                surface.errors += 1;
                                                if renderer::is_device_lost(&e) {
                                                    log::error!(
                                                        "engine: monitor[{}] present device lost: {e:?}; scheduling rebuild",
                                                        surface.index
                                                    );
                                                    device_lost = true;
                                                } else if surface.errors == 1
                                                    || surface.errors.is_multiple_of(150)
                                                {
                                                    log::warn!(
                                                        "engine: monitor[{}] present failed ({} total): {e:?}",
                                                        surface.index,
                                                        surface.errors
                                                    );
                                                }
                                            }
                                        },
                                        Err(e) => {
                                            surface.errors += 1;
                                            if surface.errors == 1
                                                || surface.errors.is_multiple_of(150)
                                            {
                                                log::error!(
                                                    "engine: monitor[{}] draw_nv12 failed ({} total): {e:?}",
                                                    surface.index,
                                                    surface.errors
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                for idx in indices {
                                    let surface = &mut self.monitors[idx];
                                    surface.errors += 1;
                                    if surface.errors == 1 || surface.errors.is_multiple_of(150) {
                                        log::error!(
                                            "engine: monitor[{}] decode failed ({} total): {e:?}",
                                            surface.index,
                                            surface.errors
                                        );
                                    }
                                }
                            }
                        }
                    }
                    if device_lost {
                        log::error!("engine: device lost; rebuilding wallpaper surfaces");
                        if let Err(e) = self.rebuild() {
                            log::error!(
                                "engine: fast rebuild after device loss failed: {e:?}; switching to background retry"
                            );
                            pending_rebuild = true;
                            next_rebuild_attempt = Instant::now() + REBUILD_SLOW_RETRY_DELAY;
                        }
                        next = Instant::now();
                    } else if drew_any {
                        drawn += 1;
                    } else {
                        skipped += 1;
                    }
                } else {
                    skipped += 1;
                }
                tick += 1;

                if tick.is_multiple_of(30) {
                    for surface in &self.monitors {
                        log::info!(
                            "engine: monitor[{}] group={} frames={} errors={} eos_restarts={} occupied={}",
                            surface.index,
                            surface.group,
                            surface.frames,
                            surface.errors,
                            self.groups[surface.group].decoder.eos_count(),
                            surface.occupied
                        );
                    }
                    let occ: Vec<bool> = self.monitors.iter().map(|s| s.occupied).collect();
                    log::info!(
                        "engine: tick={} drawn={} skipped={} paused={} occupied={:?}",
                        tick,
                        drawn,
                        skipped,
                        is_paused,
                        occ
                    );
                }
                if !pending_rebuild && tick.is_multiple_of(REBUILD_CHECK_TICKS) {
                    let worker_alive = workerw::is_alive(self.target);
                    let display_hint = DISPLAY_CHANGE.swap(false, Ordering::SeqCst);
                    let display_changed = self.monitors_changed();
                    if display_hint && !display_changed {
                        log::info!(
                            "engine: WM_DISPLAYCHANGE without monitor geometry change; ignoring"
                        );
                    }
                    if let Some(reason) = check_rebuild(worker_alive, display_changed) {
                        log::warn!("engine: rebuild triggered: {reason:?}");
                        if let Err(e) = self.rebuild() {
                            log::error!(
                                "engine: rebuild exhausted {REBUILD_MAX_ATTEMPTS} fast attempts: {e:?}; switching to background retry"
                            );
                            pending_rebuild = true;
                            next_rebuild_attempt = Instant::now() + REBUILD_SLOW_RETRY_DELAY;
                        }
                        next = Instant::now();
                    }
                }
                if !pending_rebuild
                    && tick.is_multiple_of(VIDEO_CHECK_TICKS)
                    && self.videos_need_reload()
                {
                    log::warn!("engine: wallpaper video changed; reloading");
                    if let Err(e) = self.rebuild() {
                        log::error!(
                            "engine: reload after video change failed: {e:?}; switching to background retry"
                        );
                        pending_rebuild = true;
                        next_rebuild_attempt = Instant::now() + REBUILD_SLOW_RETRY_DELAY;
                    }
                    next = Instant::now();
                }
            }

            let now = Instant::now();
            next = next_frame_deadline(now, next);
            if next > now {
                std::thread::sleep(next - now);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::{
        check_rebuild, file_stamp, monitor_signature, next_frame_deadline, should_draw_surface,
        RebuildReason, FRAME_INTERVAL,
    };
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::MONITORINFO;

    #[test]
    fn deadline_advances_by_one_frame() {
        let base = Instant::now();
        let deadline = next_frame_deadline(base + Duration::from_millis(5), base);
        assert_eq!(deadline, base + FRAME_INTERVAL);
    }

    #[test]
    fn deadline_clamps_when_behind() {
        let base = Instant::now();
        let now = base + Duration::from_millis(100);
        assert_eq!(next_frame_deadline(now, base), now);
    }

    #[test]
    fn file_stamp_reports_size_and_mtime() {
        let dir = std::env::temp_dir().join(format!("ad_stamp_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("a.mp4");
        std::fs::write(&file, b"12345").unwrap();
        let (len, mtime) = file_stamp(file.to_str().unwrap());
        assert_eq!(len, 5);
        assert!(mtime > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn surface_draw_respects_pause_force_and_occupancy() {
        assert!(should_draw_surface(false, false, false));
        assert!(!should_draw_surface(false, false, true));
        assert!(should_draw_surface(false, true, true));
        assert!(!should_draw_surface(true, true, false));
    }

    #[test]
    fn rebuild_reason_prioritizes_worker_loss() {
        assert_eq!(check_rebuild(false, false), Some(RebuildReason::WorkerLost));
        assert_eq!(check_rebuild(false, true), Some(RebuildReason::WorkerLost));
        assert_eq!(
            check_rebuild(true, true),
            Some(RebuildReason::DisplayChanged)
        );
        assert_eq!(check_rebuild(true, false), None);
    }

    #[test]
    fn monitor_signature_tracks_rects() {
        let mut primary = MONITORINFO::default();
        primary.rcMonitor = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let mut secondary = MONITORINFO::default();
        secondary.rcMonitor = RECT {
            left: 1920,
            top: 0,
            right: 3840,
            bottom: 1080,
        };
        assert_eq!(
            monitor_signature(&[primary, secondary]),
            vec![(0, 0, 1920, 1080), (1920, 0, 3840, 1080)]
        );
        assert_ne!(
            monitor_signature(&[primary]),
            monitor_signature(&[primary, secondary])
        );
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_DISPLAYCHANGE {
        log::info!("engine: WM_DISPLAYCHANGE received");
        DISPLAY_CHANGE.store(true, Ordering::SeqCst);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe extern "system" fn monitor_cb(
    hmon: HMONITOR,
    _hdc: HDC,
    _rc: *mut RECT,
    lp: LPARAM,
) -> BOOL {
    let out = &mut *(lp.0 as *mut Vec<MONITORINFO>);
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(hmon, &mut mi).as_bool() {
        out.push(mi);
    }
    BOOL(1)
}

unsafe fn enum_monitors() -> Result<Vec<MONITORINFO>> {
    let mut out: Vec<MONITORINFO> = Vec::new();
    let ok = EnumDisplayMonitors(
        HDC::default(),
        None,
        Some(monitor_cb),
        LPARAM(&mut out as *mut Vec<MONITORINFO> as isize),
    );
    if !ok.as_bool() {
        bail!(
            "EnumDisplayMonitors failed: {}",
            windows::core::Error::from_win32()
        );
    }
    if out.is_empty() {
        bail!("no monitors detected");
    }
    Ok(out)
}

unsafe fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = GetClassNameW(hwnd, &mut buf);
    if n <= 0 {
        return format!(
            "<GetClassNameW failed: {}>",
            windows::core::Error::from_win32()
        );
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

unsafe fn window_rect(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    let _ = GetWindowRect(hwnd, &mut r);
    r
}

fn fmt_rect(r: RECT) -> String {
    format!(
        "({},{})-({},{}) {}x{}",
        r.left,
        r.top,
        r.right,
        r.bottom,
        r.right - r.left,
        r.bottom - r.top
    )
}
