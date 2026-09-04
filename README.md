# Hotaru

Komari（[komari-monitor/komari](https://github.com/komari-monitor/komari)）的 macOS / Windows 托盘监控客户端。基于 Tauri 2（Rust + 系统 WebView），安装包小、常驻内存低。

![platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows-blue) ![license](https://img.shields.io/badge/license-GPL--3.0-green)

## 功能

- **主窗口内嵌 Komari 官方面板**：webview 直接加载你配置的后端地址，功能与浏览器一致；注入的自定义标题栏提供后退 / 前进 / 刷新与窗口控制；页面长时间加载不完会自动重载、必要时重建 webview（白屏自愈）；关闭窗口仅隐藏到托盘。
- **托盘图标实时状态**：单色“状态环 + 活动脉冲”——完整状态环 = 正常，右上圆点 = CPU/内存超过阈值或部分节点离线，斜杠 = 全部离线或后端不可达；macOS 使用 Template Image 自动反色，Windows 根据系统主题切换深浅前景。
  - 悬浮提示显示在线数、CPU、内存、实时上下行速率。
  - **macOS 菜单栏文字**：图标旁显示 `↑xxx ↓xxx` 实时速率（Windows 因系统限制仅显示图标）。
- **左键弹窗**：贴着托盘图标弹出的小面板。
  - 顶部标题旁显示在线 / 离线 / 总节点数（覆盖全部节点，不受下方筛选影响）；下面是汇总上下行速率曲线，区间可选 5 分钟 / 15 分钟 / 1 小时 / 6 小时，悬停显示该时刻的时间与 ↑↓ 速率。
  - 下方按节点列出在线状态、地区、系统与标签；展开后显示 CPU / 内存 / 硬盘 / 流量（含限额）、运行时长与到期剩余、累计上传下载，以及最近 1 小时的网络质量（延迟与丢包，12 格 × 5 分钟）。
  - 汇总与列表之间有搜索框，右侧漏斗图标按钮弹出标签筛选、排序图标按钮弹出排序菜单：按名称或标签搜索，点标签只看带该标签的节点（多选取并集）；可按名称 / 系统 / CPU / 内存 / 带宽 / 流量 / 延迟 / 丢包 / 到期时间排序，再点同一项换升降序，默认名称升序。Esc 分三级——先收浮层、再清筛选、最后才收起面板。
  - 可按住标题栏拖动；点“固定”后失焦不再自动收起。
  - 高度自适应内容，上限是托盘图标那一侧屏幕剩下的空间（按工作区算，排除任务栏/Dock）；节点多到放不下时列表可直接滚动，不显示滚动条。
- **右键菜单**：打开面板、刷新面板、设置…、开机自启、退出。
- **配置**：后端地址、API Key、刷新间隔、CPU/内存告警阈值、曲线区间、节点显示（勾选哪些节点进弹窗列表，支持全选/全部取消）、跟随系统/亮色/暗色主题、自签名证书容忍、开机自启。
- **检查更新**：设置窗口“关于”里比对 GitHub 最新 Release，可直接跳到下载页。

## 数据来源（Komari API）

| 用途 | 接口 |
|---|---|
| 实时快照 | 公开站点使用 WebSocket `/api/clients`（发送 `get` 一次拿回全部节点），失败时自动回退 HTTP；配置 API Key 时改用带 Bearer 的 `GET /api/nodes` + `GET /api/recent/{uuid}` 轮询（每轮最多 8 个请求并发），确保隐藏节点返回完整信息 |
| 节点名称/规格 | `GET /api/nodes`（每 60 秒刷新，设置里手动「获取节点列表」会立刻重读一次；节点标签取自其中的 `tags`，Komari 以 `;` 分隔） |
| 网络质量 | `GET /api/records/ping?uuid={uuid}&hours=1`（引擎每 60 秒在后台为全部节点刷新，最多 8 个并发；延迟与丢包随快照下发，展开节点即有，不再点开才请求） |
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

设置窗口源码在 `ui/index.html`、托盘左键弹窗在 `ui/chart.html`（纯 HTML/JS，无前端构建步骤）；Rust 源码在 `src-tauri/src/`：

| 文件 | 职责 |
|---|---|
| `models.rs` | Komari 线上格式解析、快照聚合、图标状态机、格式化（含单元测试） |
| `monitor.rs` | WebSocket 实时会话 + HTTP 轮询降级、事件广播 |
| `tray.rs` | tiny-skia 动态图标绘制、托盘菜单、tooltip、macOS 菜单栏文字 |
| `windows.rs` | 面板窗口（加载后端 URL、注入标题栏、白屏看门狗）、弹窗定位与尺寸、设置窗口管理 |
| `commands.rs` | Tauri 命令（读写设置、测试连接、ping 代理、检查更新、自启等） |
| `state.rs` | 运行时共享状态、内存中的网络历史（固定保留 6 小时） |

运行测试：`cargo test`（在 `src-tauri/` 下）。

## 打包发布

推 tag `v*` 或手动触发 GitHub Actions（`.github/workflows/release.yml`），矩阵构建：

- macOS：`aarch64-apple-darwin`（Apple Silicon）与 `x86_64-apple-darwin`（Intel）→ `.dmg`
- Windows：`x86_64-pc-windows-msvc` → NSIS `.exe` 安装包

## 许可证

[GPL-3.0](LICENSE)

## 已知限制

- Windows 托盘无法显示文字速率（系统限制），信息见悬浮提示与左键弹窗。
- 速率曲线只存在内存里（固定保留 6 小时），退出应用即清空；换后端地址或 API Key 保存后也会立即清空并重新积累（旧站点的节点与曲线不会残留）；时间粒度跟随刷新间隔。
- 累计流量取 Komari 上报的 `network.total_up` / `total_down`，不是“当日”用量；网络质量依赖后端已配置 ping 监控任务。
