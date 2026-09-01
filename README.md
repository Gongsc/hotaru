# Hotaru

Komari（[komari-monitor/komari](https://github.com/komari-monitor/komari)）的 macOS / Windows 托盘监控客户端。基于 Tauri 2（Rust + 系统 WebView），安装包小、常驻内存低。

![platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows-blue) ![license](https://img.shields.io/badge/license-GPL--3.0-green)

## 功能

- **主窗口内嵌 Komari 官方面板**：webview 直接加载你配置的后端地址，功能与浏览器一致；关闭窗口仅隐藏到托盘。
- **托盘实时状态**：
  - 图标采用单色“状态环 + 活动脉冲”：完整状态环 = 正常，右上圆点 = CPU/内存超过阈值或部分节点离线，斜杠 = 全部离线或后端不可达；macOS 使用 Template Image 自动反色，Windows 根据系统主题切换深浅前景。
  - 悬浮提示显示在线数、CPU、内存、实时上下行速率。
  - 菜单列出每个节点的 CPU / 内存 / ↑下载 / ↓上传明细，支持一键打开面板、设置、切换数据源、开机自启、退出。
  - **macOS 菜单栏文字**：图标旁显示 `↑xxx ↓xxx` 实时速率（Windows 因系统限制仅显示图标）。
- **数据源可切换**：汇总全部节点 / 固定关注某个节点。
- **配置**：后端地址、API Key、刷新间隔、告警阈值、跟随系统/亮色/暗色主题、自签名证书容忍、开机自启。

## 数据来源（Komari API）

| 用途 | 接口 |
|---|---|
| 实时快照 | 公开站点使用 WebSocket `/api/clients`（发送 `get` 拉取），失败时自动回退 HTTP；配置 API Key 时直接使用带 Bearer 的 `GET /api/nodes` + `GET /api/recent/{uuid}` 轮询，确保隐藏节点返回完整信息 |
| 节点名称/规格 | `GET /api/nodes`（每 60 秒刷新） |
| 连接测试 | `GET /api/nodes` + `GET /api/version` |

- 公开站点无需认证即可使用；**私有站点**（开启 `private_site`）需要 API Key：管理后台 → 设置 → API Key（≥12 位），本应用以 `Authorization: Bearer <key>` 携带（API Key 同时绕过服务端 CORS/WS Origin 校验）。
- 实时数据粒度跟随 Komari Agent 上报间隔（默认 3 秒），应用刷新间隔默认 3 秒、可配 1–60 秒。

## 开发

依赖：Rust stable（Windows 需 MSVC，macOS 需 Xcode CLT）、WebView2（Win11 内置）。

```bash
# Windows / macOS 通用
cargo install tauri-cli --locked --version "^2"
cargo tauri dev      # 开发运行
cargo tauri build    # 产出安装包
```

设置窗口源码在 `ui/index.html`（纯 HTML/JS，无前端构建步骤）；Rust 源码在 `src-tauri/src/`：

| 文件 | 职责 |
|---|---|
| `models.rs` | Komari 线上格式解析、快照聚合、图标状态机、格式化（含单元测试） |
| `monitor.rs` | WebSocket 实时会话 + HTTP 轮询降级、事件广播 |
| `tray.rs` | tiny-skia 动态图标绘制、托盘菜单、tooltip、macOS 菜单栏文字 |
| `windows.rs` | 面板窗口（加载后端 URL）、设置窗口管理 |
| `commands.rs` | Tauri 命令（读写设置、测试连接、自启等） |

运行测试：`cargo test`（在 `src-tauri/` 下）。

## 打包发布

推 tag `v*` 或手动触发 GitHub Actions（`.github/workflows/release.yml`），矩阵构建：

- macOS：`aarch64-apple-darwin`（Apple Silicon）与 `x86_64-apple-darwin`（Intel）→ `.dmg`
- Windows：`x86_64-pc-windows-msvc` → NSIS `.exe` 安装包

## 许可证

[GPL-3.0](LICENSE)

## 已知限制

- Windows 托盘无法显示文字速率（系统限制），信息见悬浮提示与菜单。
- 菜单每次数据刷新都会重建（约每秒一次）：菜单展开期间偶尔会收起，再次点击即可。
- Komari API 无"当日流量"接口，菜单/提示中为实时速率；累计流量为本次会话内累计。
