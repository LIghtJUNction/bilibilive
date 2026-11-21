# 更新与 OBS 组件

bilibilive 将版本检查、下载和应用拆成三个明确步骤。打开 **偏好设置 → 更新与组件**，可以查看当前版本、最新稳定版本、安装来源和本地 OBS 插件状态。所有入口调用同一套共享操作，结果中的状态与错误也可通过 CLI、MCP 或本地 HTTP 读取。

## 桌面更新

普通 GUI 启动时按 `update_check` 偏好检查一次稳定版本。新配置默认开启，已有配置保存的关闭状态继续保留；设置页可以随时修改。`bilibilive gui --noupdate` 只暂停本次启动检查。演示和只读启动都跳过自动检查，只读模式仍允许点击“检查新版本”。

发现新版本后，侧栏会保留更新提示。点击“下载更新”才开始下载，进度显示在更新页；下载完成后继续等待“应用并重启”。应用前会再次验证这次检查选定的版本与文件，释放本地 OBS 观察器和 RTC／PK 媒体连接，再正常退出窗口。运行中的 OBS 和 Bilibili 直播间保留原状态。重启打开 GUI 并保留配置目录，不重复执行原来的开播、MCP 或其他命令。

应用失败时窗口保留错误详情。如果本地媒体连接已释放但安装助手未成功启动，界面会明确显示这个状态，停止自动重连。可根据错误处理后重试更新；重新连接媒体需要用户明确操作。

## 安装渠道决定更新方式

| 来源 | 更新行为 |
| --- | --- |
| Windows/macOS 安装器 | 识别 Velopack 安装信息和匹配的更新助手，校验固定发布版本的渠道与包摘要，再允许下载和应用 |
| Cargo | 显示与当前安装前缀、版本对应的 Cargo 更新命令，供用户复制执行 |
| Arch / AUR | 保留包管理器的文件归属，显示对应包的升级方法 |
| 便携归档或来源未知 | 提供正式发布页面和手动安装提示，保留现有可执行文件 |

Windows 安装器使用 `bilibilive-win-x64-Setup.exe`；macOS 按架构提供 `bilibilive-osx-x64-Setup.pkg` 和 `bilibilive-osx-arm64-Setup.pkg`。对应更新渠道是 `win-x64`、`osx-x64` 和 `osx-arm64`，各自拥有独立的 `releases.<channel>.json`。便携包仍按便携安装处理。实际可下载的安装器、应用归档与媒体组件以[该版本的正式发布资产](https://github.com/LIghtJUNction/bilibilive/releases)为准；早期版本的便携归档不会自动获得安装器身份。

软件检查只读取官方发布元数据。Bilibili 请求常量与签名配置随经过审阅的源码一起发布，不从远端 JSON 热替换。上游三个更新任务的具体触发关系记录在 [StartLive 审计](STARTLIVE_AUDIT.md#update-triggers-and-compatibility-choices)。

## CLI、MCP 与共享会话

只检查最新版本或本地组件状态，可以使用一次性命令：

```sh
bilibilive call updates.check --args '{}'
bilibilive --read-only call updates.check --args '{}'
bilibilive call obs.plugin.status --args '{}'
```

检查结果中的 `installation.kind` 标识安装来源，`manual_command` 提供适用的手动更新命令。只有受支持的安装器身份才会返回可用的 `staged_id` 和下载／应用能力。`staged_id` 将后续操作绑定到同一次检查的版本、包摘要和安装身份；重复检查或版本变化后，应使用最新结果中的值。

下载进度与待应用包由运行中的 App 会话持有。CLI 多次操作应连接同一个 `gui --listen` 或 `serve` 进程：

```sh
bilibilive gui --listen 127.0.0.1:18779
# 在另一个终端设置启动时显示的访问 token。
export BILIBILIVE_API_TOKEN='启动时显示的 token'
bilibilive --endpoint http://127.0.0.1:18779 call updates.check --args '{}'
bilibilive --endpoint http://127.0.0.1:18779 call updates.status --args '{}'
```

各操作的参数和后续步骤如下。`staged_id` 使用上一条检查结果的原值。

| 操作 | 参数 | 结果与下一步 |
| --- | --- | --- |
| `updates.check` | `{}` | 检查最新稳定版，返回渠道、版本和可用能力 |
| `updates.status` | `{}` | 读取当前会话状态与进度，不触发网络检查 |
| `updates.download` | `{"staged_id":"检查结果的值","wait":false}` | 在当前持久会话后台下载；用 `updates.status` 查看进度 |
| `updates.apply` | `{"staged_id":"检查结果的值","restart":true}` | 校验包并启动安装助手，发送成功回执后宿主正常退出 |

GUI 的“应用并重启”会完成窗口退出。通过本地 HTTP 或 MCP 请求 `updates.apply` 时，宿主先发送并刷新成功回执，再结束服务；连接同一 App 的窗口随后正常退出。回执中的 `exit_required` 和 `exit_timeout_seconds` 描述安装助手等待的退出条件。启动钩子不会在下次运行时自动应用此前下载的更新。

zuse 脚本中的 `updates.apply` 必须是展开后计划的最后一步，预检会拒绝它之后的操作。成功时，脚本的完成回执携带相同的退出信息，由 GUI、HTTP 或 MCP 完成正常退出；失败时保留结构化步骤结果和窗口。本地会话关闭只完成了一部分时，也会停止自动观察与重连，等待用户查看错误后处理。

## 安装 OBS PK 组件

更新与组件页先检查随包清单、插件版本、目标平台及逐文件摘要。有效 bundle 可通过“安装随包组件”安装到 OBS 插件目录；默认保留已有文件，替换需要单独开启。安装过程只复制经过校验的本地组件，不下载、不编译、不提升权限，也不关闭正在运行的 OBS。安装后请手动重启 OBS。

```sh
bilibilive call obs.plugin.status --args '{}'
bilibilive call obs.plugin.install --args '{"replace":false}'
```

`installed` 表示文件已存在，`verified` 表示相应校验结果，`restart_required` 提醒重启 OBS。OBS 是否加载插件应查看 OBS 日志；这里的文件状态不读取 OBS 模块加载结果。安装目录不可写、已有文件或缺少有效随包组件时，返回原因与处理提示。

Cargo 源码构建若没有匹配组件 bundle，会返回 `unavailable`。Arch 用户可以安装 `bilibilive-obs-plugin` 或 `bilibilive-obs-plugin-git`；其他源码构建方法见[插件说明](../plugins/obs-bilibilive-pk/README.md)。高级入口接受显式 `bundle_root` 和 `manifest_sha256`，二者必须配对使用；默认入口使用发行包绑定的清单摘要。

## 验证范围

GUI 状态夹具覆盖启动开关、只读／演示边界、手动下载与应用分离、进度轮询、失败后保留窗口、媒体释放后的重连限制，以及插件替换的显式选择。原生安装器升级、macOS／Windows 窗口交互和对应 OBS 模块加载应分别提供平台实机收据。已有 Linux 窗口与 OBS 加载记录保留在 [VALIDATION.md](VALIDATION.md)，继续归属于它们的原始源码快照。
