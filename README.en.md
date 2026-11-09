# ActiveDesktop

> English | [简体中文](README.md)

A lightweight, native live wallpaper for Windows: play MP4 videos behind your desktop icons, with per-monitor videos and per-monitor auto-freeze.

## Features

- **Video wallpaper behind desktop icons**: icons, right-click and drag-and-drop keep working.
- **Different video per monitor**: automatically assigned by filename `wallpaper1.mp4`, `wallpaper2.mp4`, ...
- **Per-monitor freeze**: a monitor freezes its current frame as soon as any app window is on it; other monitors keep playing.
- **Hardware decoding**: Media Foundation + D3D11 — low CPU on Intel/AMD/NVIDIA iGPUs and GPUs.
- **30 fps cap**, cover scaling (no black bars), silent.
- **Multi-monitor**: rebuilds automatically on hot-plug or resolution change.
- **Optional autostart** (`HKCU\...\Run`), no tray, no UI.
- **Single-process design**: no explorer injection; a crash only affects itself and a restart recovers.

## How it works

Windows hosts the desktop in `Progman`. An undocumented message (`0x052C`) can split it into an icon layer (`SHELLDLL_DefView`) and a wallpaper layer (`WorkerW`).
ActiveDesktop parents a D3D11 swap-chain window under `WorkerW`, below the icons, and decodes the video with Media Foundation.
Per-monitor occupancy is computed by enumerating top-level windows and mapping them via `MonitorFromWindow`.

## Requirements

- Windows 10 x64 (developed on 19045).
- Rust toolchain (`x86_64-pc-windows-msvc`) + VS Build Tools.
- H.264 / MP4, 8-bit `yuv420p`, 1080p 30 fps recommended.

## Build

```bat
cargo build --release
```

## Usage

Portable: put videos next to the executable:

```
ActiveDesktop.exe
wallpaper1.mp4      # screen 1
wallpaper2.mp4      # screen 2 (falls back to wallpaper1.mp4, then wallpaper.mp4)
```

Run:

```bat
ActiveDesktop.exe                 :: use wallpaper1/2/... per monitor
ActiveDesktop.exe video.mp4       :: force one video on every monitor
ActiveDesktop.exe --install       :: register autostart
ActiveDesktop.exe --uninstall     :: remove autostart
```

`--install` copies `ActiveDesktop.exe` to `%LOCALAPPDATA%\ActiveDesktop` and registers autostart from that copy; put `wallpaper1.mp4`, `wallpaper2.mp4`... into that directory. `--uninstall` removes only the autostart entry; program files are kept.

Terminate the process: Task Manager.

## Behavior

- A monitor **freezes on its current frame** when any non-minimized app window is on it; it resumes when windows move away or minimize.
- Monitors using the same video file share one decode; different files decode independently.
- Freezing only stops presenting on that monitor (last frame is kept); on resume it presents the group's latest frame, immediately aligned with the others.
- Render cap 30 fps; lower source fps is not padded.
- Scaling is **cover** (preserve aspect, crop overflow, no black bars).
- On hot-plug or resolution change, `wallpaperN.mp4` is re-resolved and the wallpaper is rebuilt.

## Known limitations

- Screen numbers follow the system enumeration order; rearranging or hot-plugging displays may map `wallpaperN.mp4` to a different physical screen.
- No DPI awareness yet; mixed-DPI multi-monitor setups may be sized incorrectly.
- Hot-plug triggers a full rebuild (brief black flash).
- `WorkerW` relies on undocumented behavior and may break on Windows updates.
- Single process: the wallpaper shares the application's lifetime; an abnormal exit requires a manual restart.

## Contributing

Issues and PRs are welcome. Please run `cargo test` before submitting.

## References & Acknowledgements

This is an original Rust implementation. The following open-source projects
were consulted for design ideas and algorithms; except where noted, no source
code was copied. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

- [heraklessii/Muivly](https://github.com/heraklessii/Muivly) (Apache-2.0): WorkerW dual layout and size validation, liveness check, per-window occupancy, Media Foundation/D3D11 hardware-decode flow.
- [Shogun65/Worker-Finder](https://github.com/Shogun65/Worker-Finder) (MIT): `0x052C` argument combinations and WorkerW lookup.
- [wangwenx190/dynamic-desktop](https://github.com/wangwenx190/dynamic-desktop) (GPL-3.0): concept only (parenting a window under WorkerW as wallpaper); no code copied.
- [rocksdanister/lively](https://github.com/rocksdanister/lively): general architecture inspiration.

## License

MIT License.

## Disclaimer

This project relies on undocumented Windows behavior. It is provided "as is", without warranty. Use at your own risk.
