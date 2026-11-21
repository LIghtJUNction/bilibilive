# bilibilive

**用 Rust 构建的哔哩哔哩直播控制台。** 原生桌面、命令行、MCP 和 zuse 使用同一套账号、直播间与媒体操作。桌面界面基于 [zgpui](https://github.com/LIghtJUNction/zgpui)，命令行使用 [zclap](https://github.com/LIghtJUNction/zclap)，MCP 使用 [zmcpio](https://github.com/LIghtJUNction/zmcpio)，脚本由 [zuse](https://github.com/LIghtJUNction/zuse) 的原生解析器处理。

[安装](#安装) · [更新与组件](docs/UPDATES.md) · [命令行](#命令行) · [MCP](#mcp) · [zuse](#zuse-原生脚本) · [兼容范围](docs/COMPATIBILITY.md) · [StartLive 功能审计](docs/STARTLIVE_AUDIT.md)

## 功能与入口

bilibilive 以 [Radekyspec/StartLive](https://github.com/Radekyspec/StartLive) 的完整功能集为兼容目标。已审计的参考提交是 [`0b5a9830`](https://github.com/Radekyspec/StartLive/tree/0b5a9830a8305d9cd6ac15378e46bee05b894c11)。当前实现和验收边界逐项记录在[兼容说明](docs/COMPATIBILITY.md)中；真实账号权限、双端 PK 媒体及各操作系统实机表现需要各自的验收证据。

| 场景 | 入口与行为 |
| --- | --- |
| 账号 | 扫码登录、Cookie 导入、多账号切换、账号验证、票据刷新；凭据交给操作系统钥匙串 |
| 直播间 | 标题、分区、公告、封面预览与 16:9 裁剪、审核轮询、每账号最近五个标题、最近分区、平台许可与推流延迟 |
| 开停播 | RTMP、SRT 优先并允许 RTMP 回退、仅使用 SRT；平台要求验证时展示原始验证流程 |
| OBS | WebSocket v5 连接与认证、配置推流地址、启动和停止推流、开停播联动 |
| PK 与连线 | 普通 PK、视频 PK、视频连线、主播搜索、邀请与接收、PK 轮次和设置接口 |
| 直播事件 | 原生弹幕连接、压缩帧解析、心跳与序列确认；从邀请事件取得对方身份与会话 |
| 原生媒体 | 基于系统 libdatachannel 的 RTC、发布和订阅、FFmpeg 采集与预览；配套 OBS 原生插件源码 |
| 配置转换 | 发现直播姬配置，导出新的 OBS 场景集合，保留输入文件并报告不支持的来源 |
| 自动化 | JSON 命令、stdio／本地 HTTP MCP、zuse 函数和模块、整段脚本预检与有界执行 |
| 桌面 | 深浅主题、系统字体、本地背景图片及模糊／透明度、系统托盘、关闭到托盘、同配置单实例唤醒 |
| 更新与组件 | 启动版本提示、显式下载与应用、安装来源识别；校验并安装随包 OBS 插件，保留包管理器的更新职责 |
| 直播接口配置 | 从 StartLive 官方配置刷新公开协议参数，保存经过验证的本地缓存；后续请求统一采用新的签名与请求头配置 |

桌面主界面保留直播时常用的操作。**偏好设置 → 完整操作**提供同一份操作目录的高级入口，包含配置转换、连续 PK、媒体发布／接收以及其他已注册操作。每个操作都能查看参数定义，并返回结构化结果。

封面页显示当前封面，也可读取本地 PNG、JPEG 或 WebP。拖动或缩放选区后，程序在后台导出 704 × 396 PNG，再调用同一上传接口；本地原图保持不变。**偏好设置 → 字体、背景与托盘**管理外观和窗口行为。Linux 托盘使用桌面的 StatusNotifier 服务；桌面不提供该服务时，窗口仍可独立使用。

**偏好设置 → 更新与组件**显示当前版本、安装来源和 OBS 插件文件状态。版本检查只获取发布信息；下载和应用各有独立按钮。应用更新会退出并重新打开窗口，先释放本地媒体连接；正在运行的 OBS 和 Bilibili 直播间保持原状态。带有完整 Velopack 元数据和更新助手的安装器版本、Portable.zip 都可使用应用内更新；Cargo、AUR 与普通裸程序归档按各自来源手动更新。

## 安装

### crates.io

```sh
cargo install bilibilive --locked
bilibilive gui
```

仅使用 CLI、MCP 和自动化的主机可以省去桌面编译：

```sh
cargo install bilibilive --locked --no-default-features
bilibilive doctor
```

构建需要 Rust **1.100 或更新版本**。Linux 原生界面使用 X11／XWayland、FreeType、fontconfig 和 OpenGL；安装发行版对应的开发包后再通过 Cargo 构建。需要 CPU 绘制的 Linux 环境可以使用 `--features linux-software`。

### Arch Linux / AUR

```sh
yay -S bilibilive
```

跟随当前开发分支与最新 Project Z 源码：

```sh
yay -S bilibilive-git
```

OBS 的 PK 采集插件单独打包为 `bilibilive-obs-plugin` 和 `bilibilive-obs-plugin-git`。需要这组能力时安装对应插件，同时准备 `obs-studio`、`libdatachannel` 和 `ffmpeg`。AUR 使用独立的 makepkg 构建目录，以保持打包隔离；日常开发继续使用本机配置的共享 Cargo target。

### GitHub Releases 与源码

[GitHub Releases](https://github.com/LIghtJUNction/bilibilive/releases)提供正式构建及其 SHA-256 文件。Linux、macOS 和 Windows 的构建默认包含原生 GUI；Linux 使用 X11／XWayland，Windows 使用 Win32，macOS 使用 AppKit。具体版本的架构、便携归档和安装器以该版本资产列表为准。已有窗口运行验收来自 Linux；macOS 和 Windows 的交互与 OBS 加载验收单独记录，详见[兼容说明](docs/COMPATIBILITY.md)。

```sh
git clone https://github.com/LIghtJUNction/bilibilive.git
cd bilibilive
cargo build --release --locked
```

### 更新设置

新配置默认在普通 GUI 启动时刷新一次直播接口配置，再检查稳定版本；已有配置中关闭的 `update_check` 会继续保留。可在更新页或 **偏好设置 → 直播接口配置** 关闭该偏好，或仅为本次启动传入 `--noupdate`：

```sh
bilibilive gui --noupdate
bilibilive call updates.check --args '{}'
bilibilive call obs.plugin.status --args '{}'
bilibilive protocol status
bilibilive protocol refresh
```

接口配置刷新只读取上游官方 JSON，不携带账号 Cookie。经过类型和内容验证后，新配置原子写入本地缓存；网络或校验失败时保留当前有效配置。在途请求和已有媒体连接继续使用原配置。只读模式允许查看配置和手动检查软件版本，禁止刷新缓存；演示模式使用离线数据。安装器更新和共享会话的具体命令见[更新与组件说明](docs/UPDATES.md)。

## 开始直播

启动 `bilibilive` 或 `bilibilive gui`，在账号页生成二维码，使用哔哩哔哩 App 扫码并确认。选择直播分区、填写标题后即可发出开播请求。推流地址页分别提供服务器地址和密钥；密钥默认隐藏，复制或显示由用户显式触发。

命令行提供相同流程：

```sh
bilibilive login --name daily
bilibilive room get
bilibilive area list
bilibilive room update --title '今天一起写代码' --area 371
bilibilive start --protocol srt --strategy fallback
bilibilive stream --show-secrets
bilibilive stop
```

`stream` 查询现有推流地址。`start` 才会发出开播请求。选择严格 SRT 后，如果平台已经开播但没有返回 SRT 地址，结果会明确保留“房间已开播”的状态，并提示当前协议不可用。

## 命令行

常用操作有短命令；完整能力由 `schema` 和 `call` 暴露。GUI、CLI、MCP 和 zuse 最终调用同一个 `App`，因此账号选择、参数检查和业务行为保持一致。

```sh
bilibilive --help
bilibilive schema
bilibilive doctor --json
bilibilive call room.get --args '{}'
bilibilive call delay.set --args '{"seconds":10}'
bilibilive account list
bilibilive account use --name daily
bilibilive room cover --path /path/to/cover.png
```

对于包含密码或较长 JSON 的请求，`--args -` 从标准输入读取。CLI 默认隐藏结果中的密钥；只有显式指定 `--show-secrets` 才打印它们。无桌面钥匙串的服务器可以通过 `--cookie-file /path/to/cookie` 临时提供登录状态，或使用 `BILIBILIVE_COOKIE`。

`--config-dir /path/to/workspace` 选择独立配置目录。`--read-only` 从共享派发入口禁止会改变账号、直播间、媒体或配置的操作，适合诊断与只读代理。

### 连接同一个持续会话

RTC、媒体预览、弹幕连接和 OBS 观察器由创建它们的进程持有。一次普通 CLI 命令结束后，本进程的这些资源随之关闭。连续执行 `rtc.connect`、`rtc.publish`、`rtc.subscribe` 等操作时，使用 `--endpoint` 将每条命令交给同一个持续运行的桌面或服务。

先在一个终端启动带本地接口的桌面：

```sh
bilibilive gui --listen 127.0.0.1:18779
```

启动时 stderr 会显示访问 token。另一个终端将该值设置为 `BILIBILIVE_API_TOKEN`，然后连接同一会话：

```sh
export BILIBILIVE_API_TOKEN='启动时显示的 token'
bilibilive --endpoint http://127.0.0.1:18779 account list
bilibilive --endpoint http://127.0.0.1:18779 login --name daily
bilibilive --endpoint http://127.0.0.1:18779 rtc status
bilibilive --endpoint http://127.0.0.1:18779 call pk.session --args '{}'
bilibilive --endpoint http://127.0.0.1:18779 zuse run studio.zuse
```

没有桌面的主机可用 `bilibilive serve --listen 127.0.0.1:18779` 承载同一组操作。已有环境变量 `BILIBILIVE_API_TOKEN` 时，启动进程使用该值；客户端必须提供相同值。端点接受 `http://127.0.0.1:端口` 或 `http://[::1]:端口` 这样的回环 IP 地址，且不经过代理或重定向。

端点使用服务进程选中的账号与配置。`--config-dir` 应传给启动该服务的命令。`login`、`call`、普通命名命令和 zuse 均可通过 `--endpoint` 使用同一会话；zuse 的相对模块路径以脚本所在目录为基准，由同一台机器上的服务读取。`mcp`、`serve` 和 `gui` 各自创建本地会话，HTTP MCP 客户端可连接已运行窗口的 `/mcp` 路径。

本地预览有独立的前台命令，保持运行直到按下 Ctrl-C：

```sh
bilibilive rtc preview-local
bilibilive rtc preview-local --arg audio=true --arg volume=40
```

第一条仅预览 OBS 画面；音频监听需要第二条中的显式 `audio=true`。该命令使用配套 OBS 采集插件和 FFplay，停止时关闭预览并释放其采集需求。也可为它指定 `--endpoint`，在既有桌面会话中打开预览。

### OBS

在 OBS 中启用 WebSocket 服务器，再通过界面保存连接信息，或使用命令行：

```sh
bilibilive call obs.configure --args -
bilibilive obs status
bilibilive obs sync
bilibilive obs start
bilibilive obs stop
```

`obs.configure` 接受 `url` 和可选 `password`。省略密码会保留已有密码；显式传入空密码会清除它。OBS 密码存放在操作系统凭据库中。桌面 OBS 页可配置开停播联动、自动保持连接，以及 OBS 推流时联动开播。设置了 `obs_auto_connect`、`obs_link` 或 `obs_auto_stop` 后，桌面持续观察 OBS 状态，并在连接中断后重试。

纯 CLI 前台观察使用 `bilibilive obs watch`；指定 `--endpoint` 可观察已有会话中的 OBS。也可通过该端点调用 `obs.watch.start`、`obs.watch.status` 和 `obs.watch.stop` 管理后台观察器。

原生 OBS 采集插件位于 [`plugins/obs-bilibilive-pk`](plugins/obs-bilibilive-pk)。插件通过 OBS 的 C++ 宿主接口提供本机音视频，由 Rust 媒体工作线程发送；接收对方媒体后使用 FFplay 原生预览。插件构建需要 OBS、Qt 6 和 CMake SDK。

插件沿用 StartLive 的本机媒体协议和 OBS 输出标识，便于协议互通。如果已安装原 StartLive OBS 插件，请先停用该插件，再启用 bilibilive 版本，避免同一 OBS 进程重复注册输出和占用同一采集通道。

发行包携带经过校验的匹配组件时，更新与组件页可将它安装到 OBS 插件目录。默认保留已有文件，替换需要单独开启；安装成功后手动重启 OBS。组件状态检查报告文件与校验结果，OBS 是否加载该模块仍以 OBS 日志为准。缺少随包组件的 Cargo 构建会显示具体原因；Arch 用户可直接安装对应的 AUR 插件包。

### 场景转换

```sh
bilibilive convert discover
bilibilive convert livehime --input /path/to/livehime.json --output /path/to/new-obs.json --name Studio
```

转换写入一个新的 OBS JSON 文件，输入保持不变。结果会列出未支持的来源、滤镜或缺失资源，便于在 OBS 中逐项处理。

## MCP

将 bilibilive 作为 stdio MCP 服务启动：

```json
{
  "mcpServers": {
    "bilibilive": {
      "command": "bilibilive",
      "args": ["mcp"]
    }
  }
}
```

工具名称与 `bilibilive schema` 的操作名称一致，例如 `room.get`、`live.start`、`obs.status` 和 `zuse.run`。错误会返回 MCP 的失败结果。日志写入 stderr，stdout 保留给 JSON-RPC。需要只读工具边界时，将参数改为 `["mcp", "--read-only"]`。

### 本地 HTTP

```sh
bilibilive serve --listen 127.0.0.1:18779
```

服务绑定回环地址，提供 `/mcp`、`/api/call`、`/api/startLive`、`/api/stopLive` 和 `/health`。除健康检查外，请求需要 Bearer token；可设置长度至少 24 字符的 `BILIBILIVE_API_TOKEN`，或使用启动时生成的 token。

普通 GUI 启动默认只打开窗口；显式 `gui --listen` 才启动共享 HTTP 接口。端口占用、地址或 token 无效会在窗口启动前报告。`gui --demo` 使用离线数据，拒绝启动网络监听。

StartLive 的两个旧开停播路径可通过 `--compat` 在回环地址上启用无 token 兼容模式。普通 HTTP 接口默认保留认证与来源检查。

## zuse 原生脚本

bilibilive 直接使用 zuse 的解析器和模块展开器。脚本调用共享的直播操作，不依靠窗口位置或模拟键鼠。

```sh
bilibilive zuse eval 'timer.start(); bilibilive.status(); timer.stop()'
bilibilive zuse check studio.zuse
bilibilive zuse run studio.zuse
```

一个可先检查再执行的脚本：

```text
timer.start()
bilibilive.room.update(title="今天一起写代码", area_id=371)
bilibilive.start(protocol="srt", strategy="fallback")
bilibilive.obs.start()
timer.stop()
```

执行前检查整个展开后的程序，包括语法、操作名称和参数类型。最多执行 256 个步骤，总等待上限为 60 秒。结果记录已完成步骤、失败位置与耗时；遇到平台验证或请求的推流协议不可用时，会停止后续操作。

## 最新依赖与可复现发布

开发清单中的 Project Z 依赖使用各自 GitHub 仓库的默认分支，不固定 tag、revision 或 registry 版本。`Cargo.lock` 保存这一次验证采用的精确提交；下一轮更新重新获取当前分支头。

当前开发锁采用 zconv 0.4.1、zcall 0.4.1、zmcpio 0.5.0、zuse-computer 0.4.2 和 zgpui 0.1.6 的公开源码。Zuse 0.4.2 的定向更新、精确公开提交与受影响检查记录在[更新收据](docs/evidence/dependencies-zuse-0.4.2.json)中。早期 16 个外部直接依赖和初版内部锁的检查保留在[原始依赖收据](docs/evidence/dependencies-2026-11-21.json)；正式 tag 与各次验收记录继续使用当时的不可变锁文件。

```sh
python scripts/update-dependencies.py
python scripts/update-dependencies.py --apply
```

第一条命令显示最新源码和外部稳定版本；第二条更新外部版本下界与锁文件，检查解析出的 Git 提交确实是当前默认分支，并执行本地验证。兼容性错误会直接报告，保留修改现场供修复。

仓库同时配置了 Dependabot 的定期 Rust 与 GitHub Actions 更新检查。内部 Git 依赖继续跟随默认分支，不添加版本或提交选择器；外部依赖更新版本下界。更新以分组 PR 提供，合并前运行本地验证；正式发布仍使用经过验证的锁文件和不可变标签。

crates.io 发布使用独立目录。准备脚本比较每个 Project Z 注册表包与当前锁定 Git 源码的包身份、完整随包内容及转换后的清单；同名同版本而内容不同会阻止发布。

```sh
python scripts/prepare-release.py --output /tmp/bilibilive-registry-release
```

脚本生成精确 registry 依赖和对应锁文件，验证包图没有变化，再执行 `cargo package --locked`。`release-provenance.json` 记录源提交、各依赖的 Git 提交和 `.crate` 摘要。开发清单保持原样，脚本本身不会上传包。

## 本地验证

```sh
python scripts/validate.py
python scripts/validate.py --release --obs-plugin
bilibilive gui --demo
```

普通验证使用协议夹具和本地测试，不启动真实直播、发送真实弹幕或发起真实 PK。`gui --demo` 使用标注为 DEMO 的内置数据，不访问账号服务。发布工作流仅由 tag 或手动触发；日常提交不触发远程常规 CI。

初版源码提交 `ae04b164` 于 2026-11-21 通过 Linux 原生发布验证和独立安装前缀验收。下表保留这次发布及其早期窗口／媒体快照的历史结果，不将这些计数延伸到后续功能。每项检查对应的锁文件摘要、命令和截图摘要记录在[验证收据](docs/evidence/validation-2026-11-21.json)中，详细范围见[验收说明](docs/VALIDATION.md)。

| 检查 | 历史结果 | 覆盖范围 |
| --- | --- | --- |
| 更新依赖后的无桌面 Rust 测试 | 57 项库测试、2 项 CLI 测试通过 | 包含 5 项 zuse host 测试、共享调用、账号上下文及真实本地 HTTP／OBS 协议夹具；本次未出现权限错误 |
| 最终 Linux 原生发布验证 | `validate.py --release --obs-plugin` 通过；65 项库测试、2 项 CLI 测试通过 | `ae04b164`：全部特性检查与测试、无桌面检查、默认原生 release 构建和 OBS 插件构建 |
| 独立安装前缀验收 | 接口清单含 94 项操作、35 项只读 MCP 工具；3 步 zuse 脚本执行通过 | `ae04b164`：清单枚举、`config.get` 调用、stdio MCP 握手与本地 HTTP 认证／匿名拒绝；没有逐项执行整份清单，也没有实际开播 |
| 原生窗口 | 1160 × 800 完整窗口截图人工检查通过 | 中文字形正常，窗口边缘完整，未观察到排版溢出；使用私有 Xvfb 与 Mesa OpenGL 软件渲染 |
| 自定义背景 | 60% 透明度的四个采样点均通过 | 四象限图片的位置、颜色方向与透明度合成；每通道误差不超过 5 |
| 原生 RTC 回环 | ICE／DTLS／SCTP 建连、101 号数据通道、Protobuf 请求回应、SRTP／RTP 往返通过 | 两个本机 libdatachannel 端点与本机媒体链路 |
| H.264 编码探测 | 70 ms，收到 39 个 RTP 包 | FFmpeg 合成的 128 × 128、10 fps、8 帧输入 |
| AV1 编码探测 | 403 ms，收到 33 个 RTP 包 | 相同尺寸和帧数的合成输入 |
| OBS 插件 | CMake 构建成功，OBS 33 私有实例加载成功 | 日志与 `/proc` 映射同时确认加载本次编译的模块；没有开始采集或直播 |

编码探测的耗时包含进程启动与本机收包，供检查编码器是否可用；直播分辨率、画面内容和硬件会改变实际表现。原生 RTC 夹具位于 [`src/media/native.rs`](src/media/native.rs)，编码探测位于 [`src/media/capture.rs`](src/media/capture.rs)，封面与单实例夹具位于 [`src/desktop.rs`](src/desktop.rs)。

OpenGL 截图使用 Mesa 软件实现，证明这条原生绘制路径能够运行；它不代表硬件 GPU 性能，也不等于 `linux-software` 特性的独立绘制后端已完成截图验收。最终发布验证覆盖该特性的编译与测试，截图验收仍单独记录。后续仅修改工作流和验证文档的提交沿用明确归属于 `ae04b164` 的运行证据。

真实账号的开播权限、平台身份验证、线上双端 PK，以及 macOS／Windows 实机表现仍需各自的验收记录。[兼容说明](docs/COMPATIBILITY.md)持续记录这些平台检查与已完成的本地证据。

## 开源许可

bilibilive 采用 **GPL-3.0-only**。StartLive 的协议映射和保留的 OBS 插件代码遵循原项目许可与归属说明，见 [NOTICE](NOTICE) 和[插件来源](plugins/obs-bilibilive-pk/UPSTREAM.md)。本项目图标为独立 SVG 源码，没有复制 StartLive 的受限标志。
