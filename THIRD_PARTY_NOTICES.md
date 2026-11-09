# Third-Party Notices / 第三方参考

ActiveDesktop is an original Rust implementation. The following open-source
projects were consulted for design ideas and algorithms. Except where
explicitly noted, no source code was copied verbatim; the referenced behavior
was reimplemented in Rust.

## heraklessii/Muivly
- URL: https://github.com/heraklessii/Muivly
- License: Apache-2.0
- Referenced: WorkerW dual child/sibling discovery; desktop-size validation
  against invisible stubs; WorkerW liveness check; per-window occupancy
  approach; Media Foundation + D3D11 hardware decode flow; NV12 Y/UV plane
  sampling.
- Files informed: `src/core/workerw.rs`, `src/core/occupancy.rs`,
  `src/core/decoder.rs`, `src/core/renderer.rs`.

## Shogun65/Worker-Finder
- URL: https://github.com/Shogun65/Worker-Finder
- License: MIT
- Referenced: undocumented `0x052C` argument combinations and WorkerW lookup.
- Files informed: `src/core/workerw.rs`.

## wangwenx190/dynamic-desktop
- URL: https://github.com/wangwenx190/dynamic-desktop
- License: GPL-3.0
- Referenced: concept only — parenting a playback window under WorkerW so it
  renders as the desktop wallpaper. No GPL source code was copied or adapted.
- Files informed: `src/core/engine.rs` (concept only).

## rocksdanister/lively
- URL: https://github.com/rocksdanister/lively
- License: see the repository
- Referenced: general architecture inspiration for a live-wallpaper application.

## Runtime dependencies

- `windows` — MIT OR Apache-2.0
- `anyhow` — MIT OR Apache-2.0
- `log` — MIT OR Apache-2.0
- `simplelog` — MIT OR Apache-2.0
- `winreg` — MIT

See each crate's own license for details.
