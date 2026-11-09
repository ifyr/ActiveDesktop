# ActiveDesktop

> [English](README.en.md) | 简体中文

一个轻量、原生的 Windows 动态桌面：将mp4设置为桌面动态壁纸，支持每屏不同视频、每屏独立自动冻结。

## 特性

- **视频壁纸在图标下方**：桌面图标、右键、拖放照常可用。
- **每屏不同视频**：按文件名自动分配 `wallpaper1.mp4`、`wallpaper2.mp4`……
- **每屏独立冻结**：某块屏上出现任意应用窗口就冻结该屏当前帧，其它屏继续播放。
- **硬件解码**：Media Foundation + D3D11，Intel/AMD/NVIDIA 集显独显都低 CPU。
- **30fps 上限**、cover 缩放（无黑边）、静音。
- **多显示器**：热插拔或分辨率变化后自动重建。
- **可选开机自启**（`HKCU\...\Run`），无托盘、无界面。
- **单进程设计**：不注入 explorer，崩溃只影响自身，重启即可恢复。

## 原理

Windows 桌面由 `Progman` 托管，通过未公开消息 `0x052C` 可把它拆成图标层（`SHELLDLL_DefView`）和壁纸层（`WorkerW`）。
ActiveDesktop 把一个 D3D11 交换链窗口挂到 `WorkerW` 下、压在图标之下，用 Media Foundation 硬解视频并绘制。
每屏占用状态通过枚举顶层窗口、按 `MonitorFromWindow` 归屏来判断。

## 运行环境

- Windows 10 x64（在 19045 上开发）。
- Rust 工具链（`x86_64-pc-windows-msvc`）+ VS Build Tools。
- H.264 / MP4，8-bit `yuv420p`，推荐 1080p 30fps。

## 构建

```bat
cargo build --release
```

## 使用

便携用法：把视频放在 exe 同目录：

```
ActiveDesktop.exe
wallpaper1.mp4      # 1 号屏
wallpaper2.mp4      # 2 号屏（缺失则回退 wallpaper1.mp4，再回退 wallpaper.mp4）
```

运行：

```bat
ActiveDesktop.exe                 :: 按 wallpaper1/2/... 每屏取片
ActiveDesktop.exe video.mp4    :: 强制所有屏用同一个视频
ActiveDesktop.exe --install       :: 注册开机自启
ActiveDesktop.exe --uninstall     :: 取消开机自启
```

`--install` 会把 `ActiveDesktop.exe` 复制到 `%LOCALAPPDATA%\ActiveDesktop`，并从该副本注册开机自启；把 `wallpaper1.mp4`、`wallpaper2.mp4`… 放进这个目录即可。`--uninstall` 只移除自启，不删除程序文件。

结束进程：任务管理器。

## 行为说明

- 某块屏上有任意非最小化应用窗口时，该屏**冻结在当前帧**；窗口移走/最小化后恢复。
- 同一视频文件的屏共享一份解码；不同文件的屏各自独立解码。
- 冻结只停止该屏呈现、保留末帧；恢复时呈现该组最新帧，立即与同组屏对齐。
- 渲染上限 30fps；源帧率低于 30 不补帧。
- 缩放为 **cover**（保持比例、裁掉溢出、无黑边）。
- 显示器热插拔或分辨率变化后，重新解析 `wallpaperN.mp4` 并重建。

## 已知限制

- 运行时同目录下**必须**至少包含一个 `wallpaper.mp4` 或 `wallpaper1.mp4`。
- 屏幕序号按系统枚举顺序，重排显示器或热插拔可能导致 `wallpaperN.mp4` 落到别的物理屏。
- 暂未做 DPI 感知；混合缩放的多屏可能尺寸不准。
- 热插拔是全量重建，会有短暂黑闪。
- `WorkerW` 属未公开行为，可能随 Windows 更新失效。
- 单进程：壁纸与主程序同生命周期，异常时需手动重启。

## 参与贡献

欢迎 issue 和 PR。提交前请跑：

```bash
cargo test
```

## 参考与致谢

本项目为原创 Rust 实现，但设计与算法参考了以下开源项目；除特别说明外未复制其源码，详见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

- [heraklessii/Muivly](https://github.com/heraklessii/Muivly)（Apache-2.0）：WorkerW 双布局与尺寸校验、存活检测、每屏占用判定、Media Foundation/D3D11 硬解流程。
- [Shogun65/Worker-Finder](https://github.com/Shogun65/Worker-Finder)（MIT）：`0x052C` 参数组合与 WorkerW 定位。
- [wangwenx190/dynamic-desktop](https://github.com/wangwenx190/dynamic-desktop)（GPL-3.0）：仅参考“把窗口挂到 WorkerW 当壁纸”的概念，未复制其代码。
- [rocksdanister/lively](https://github.com/rocksdanister/lively)：整体架构参考。

## 许可

MIT License。

## 免责声明

本项目依赖 Windows 未公开行为，按“原样”提供，不附带任何担保；使用风险自负。
