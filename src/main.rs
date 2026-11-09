#![windows_subsystem = "windows"]

use std::path::PathBuf;
use std::time::Duration;

use active_desktop::core::engine;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
};

const INSTALL_DIR_NAME: &str = "ActiveDesktop";
const INSTALLED_EXE: &str = "ActiveDesktop.exe";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const AUTOSTART_VALUE: &str = "ActiveDesktop";

enum Cli {
    Install,
    Uninstall,
    Run(Option<String>),
}

fn main() {
    let sink = match active_desktop::core::log::init_preferred() {
        Ok(sink) => sink,
        Err(e) => {
            eprintln!("warning: logging disabled, continuing: {e:#}");
            active_desktop::core::log::LogSink::disabled()
        }
    };
    log::info!("ActiveDesktop start ({})", sink.location());

    // Abnormal exit (panic): flush the release build's in-memory log to disk.
    let panic_sink = sink.clone();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        panic_sink.persist_failure();
    }));

    match parse_cli(std::env::args().skip(1)) {
        Cli::Install => match install_autostart() {
            Ok(dir) => {
                log::info!("install ok: {}", dir.display());
                show_message(
                    &format!(
                        "已安装到 {}，请把 wallpaper1.mp4 等视频放进该目录。",
                        dir.display()
                    ),
                    false,
                );
            }
            Err(e) => {
                log::error!("install failed: {e:?}");
                sink.persist_failure();
                show_message(&format!("安装失败：{e}"), true);
                std::process::exit(1);
            }
        },
        Cli::Uninstall => match uninstall_autostart() {
            Ok(()) => {
                log::info!("uninstall ok");
                show_message("已取消开机自启，程序文件保留。", false);
            }
            Err(e) => {
                log::error!("uninstall failed: {e:?}");
                sink.persist_failure();
                show_message(&format!("取消自启失败：{e}"), true);
                std::process::exit(1);
            }
        },
        Cli::Run(video) => {
            if let Err(e) = run(video) {
                log::error!("run failed: {e:?}");
                // Abnormal exit (error): persist buffered logs in release builds.
                sink.persist_failure();
                eprintln!("run failed: {e:?}");
                std::process::exit(1);
            }
        }
    }
}

fn parse_cli<I: Iterator<Item = String>>(args: I) -> Cli {
    let mut video: Option<String> = None;
    for arg in args {
        match arg.as_str() {
            "--install" => return Cli::Install,
            "--uninstall" => return Cli::Uninstall,
            _ => {
                if video.is_none() {
                    video = Some(arg);
                }
            }
        }
    }
    Cli::Run(video)
}

fn install_dir() -> anyhow::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| anyhow::anyhow!("环境变量 LOCALAPPDATA 未设置"))?;
    Ok(PathBuf::from(base).join(INSTALL_DIR_NAME))
}

fn write_run_value(command: &str) -> anyhow::Result<()> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let run = hkcu.open_subkey_with_flags(RUN_KEY, winreg::enums::KEY_SET_VALUE)?;
    run.set_value(AUTOSTART_VALUE, &command)?;
    Ok(())
}

fn install_autostart() -> anyhow::Result<PathBuf> {
    let dir = install_dir()?;
    std::fs::create_dir_all(&dir)?;
    let src = std::env::current_exe()?;
    let dst = dir.join(INSTALLED_EXE);
    let already =
        dst.exists() && std::fs::canonicalize(&src).ok() == std::fs::canonicalize(&dst).ok();
    if !already {
        std::fs::copy(&src, &dst)?;
    }
    let quote = '"';
    let command = format!("{quote}{}{quote}", dst.to_string_lossy());
    write_run_value(&command)?;
    log::info!("install: autostart -> {:?}", command);
    Ok(dir)
}

fn show_message(text: &str, is_error: bool) {
    let icon = if is_error {
        MB_ICONERROR
    } else {
        MB_ICONINFORMATION
    };
    unsafe {
        let _ = MessageBoxW(
            HWND::default(),
            &windows::core::HSTRING::from(text),
            windows::core::w!("ActiveDesktop"),
            MB_OK | icon,
        );
    }
}

fn uninstall_autostart() -> anyhow::Result<()> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let run = match hkcu.open_subkey_with_flags(RUN_KEY, winreg::enums::KEY_SET_VALUE) {
        Ok(run) => run,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::info!("uninstall: Run key not present; nothing to remove");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    match run.delete_value(AUTOSTART_VALUE) {
        Ok(()) => log::info!("uninstall: autostart value removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::info!("uninstall: autostart value was not present")
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

fn run(video: Option<String>) -> anyhow::Result<()> {
    let _instance = match active_desktop::core::single_instance::SingleInstance::acquire()? {
        Some(guard) => guard,
        None => {
            log::warn!("another ActiveDesktop instance is already running; exiting");
            return Ok(());
        }
    };
    let engine = engine::spawn_wallpaper(video.as_deref())?;
    log::info!("engine spawned (override={video:?}); host process idle");

    let run_secs: Option<u64> = std::env::var("ACTIVEDESKTOP_RUN_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok());
    match run_secs {
        Some(secs) => {
            log::info!("main: bounded run for {secs}s");
            std::thread::sleep(Duration::from_secs(secs));
            log::info!("main: bounded run elapsed; shutting down");
        }
        None => {
            while !engine.is_finished() {
                std::thread::sleep(Duration::from_secs(1));
            }
            log::error!("main: engine thread ended unexpectedly; exiting non-zero");
            anyhow::bail!("engine thread ended unexpectedly");
        }
    }
    drop(engine);
    Ok(())
}
