# 常驻内存优化、PDF 瘦身与 Rust/Slint 完整迁移方案

- 状态：Rust 0.2.0 正式迁移与本机发布验收已完成
- 方案日期：2026-08-24
- 完成日期：2026-08-25
- 实施顺序：PDF 瘦身 → .NET 内存验收 → Rust/Slint 原型 → Rust 正式迁移 → 发布与真实同步验收
- 适用平台：Windows x64，最低技术目标仍为 Windows Build 19041

> 第 1–8 节保留最初的分阶段决策和迁移门槛，第 9 节记录阶段性结果，第 10 节是最终正式迁移结果；如有状态冲突，以第 10 节为准。

## 1. 目标与决策

当前程序功能规模较小，但已观测到长期运行进程约 272.6 MB Private Bytes。隔离测试表明：关闭后台同步和 Toast 时，进程约 26.1 MB Private Bytes；开启完整同步后，13 秒内升至约 167.8 MB，并处于公告 PDF 下载和解析阶段。

本方案作出以下决策：

1. 第一优先级不是替换 UI 框架，而是消除 PDF 下载和全文解析产生的大对象、高峰分配与重复工作。
2. .NET 版本先保留现有业务语义、SQLite 数据和测试体系，完成可量化的内存优化。
3. Rust 原型放在最后实施，不直接替换正式版本。
4. Rust 原型使用 `windows-rs + Slint`：
   - `windows-rs`：托盘、原生菜单、Windows 通知、单实例和启动集成。
   - Slint：简单配置窗口与运行状态窗口。
   - 不采用 Tauri，不引入 WebView 常驻进程。
5. 是否迁移 Rust，以同一台机器、同一数据样本、同一验收脚本的结果决定。

## 2. 指标口径

主要指标：

- Private Bytes：判断进程实际独占和提交的内存，作为 100 MB 目标的主口径。
- Working Set：作为用户在任务管理器中感知的辅助口径。
- 同步峰值：从发起同步到同步完成期间的最大值。
- 回落值：同步完成并空闲 5 分钟后的值。
- 长期增长：连续运行 24 小时后，相对首次空闲稳定值的增长。
- 发布体积：可运行目录和安装包压缩后的大小。

阶段性验收目标：

| 指标 | .NET 优化目标 | Rust 原型目标 |
| --- | ---: | ---: |
| 后台空闲 Private Bytes | ≤ 80 MB | ≤ 50 MB |
| 同步完成后 5 分钟 Private Bytes | ≤ 100 MB | ≤ 70 MB |
| 同步峰值 Private Bytes | ≤ 160 MB | ≤ 120 MB |
| 24 小时净增长 | ≤ 10 MB | ≤ 10 MB |
| 后台空闲句柄增长 | 0 持续增长 | 0 持续增长 |

若 PDF 本身损坏、加密或超过安全限制，可以进入 `ManualReviewRequired`，但不得为了满足内存指标而静默生成错误字段。

## 3. 阶段一：PDF 瘦身

### 3.1 流式下载

将 `ReadAsByteArrayAsync` 改为有界流式下载：

- 使用 64 KiB 左右的租用缓冲区分块读取。
- 下载时同步写入同目录临时文件并增量计算 SHA-256。
- 只保留最多 4 KiB 文件头用于 `%PDF-` 和 HTML/WAF 检查。
- 若 `Content-Length` 已超过上限，读取前立即拒绝。
- 若分块读取后的累计长度超过上限，立即终止、删除临时文件并进入受控失败。
- 默认公告文件上限为 32 MiB，HTML 上限为 4 MiB；两者均通过 `AnnouncementOptions` 配置并验证。
- 成功后使用原子移动将临时文件提升为最终哈希文件名。
- 同一内容的目标文件已经存在时复用现有文件，删除本次临时文件。
- 取消、异常和伪 PDF 路径必须清理临时文件。

### 3.2 有界 PDF 文本提取

- PdfPig 从落盘文件打开，不再从完整 `byte[]`/`MemoryStream` 打开。
- 默认最多读取前 20 页；该值已用真实北交所固定样本验证，16 页会遗漏证券代码，20 页可保留全部既有关键字段。
- 默认累计文本最多 256,000 个字符。
- 每读取一页便尝试字段解析；关键字段已经足够时提前结束。
- 北京市场除日期、代码、价格、上限和时段外，还需继续查找 `FundingMode=FullCash`。
- 达到页数或字符上限但仍得到可用字段时，保存已解析证据；没有可用字段时保持 `LowConfidence`/人工核验语义。
- 解析器版本升级，避免把旧的全文哈希语义与新的有界提取语义混淆。

### 3.3 HTML 处理

- HTML 也先流式落盘并受 4 MiB 上限约束。
- 只有验证为 HTML 后才读取为字符串。
- 保持脚本/样式清除、字段证据和伪 PDF 拒绝行为不变。

### 3.4 缓存与重复工作

本阶段保留“同一 URL 内容变化必须创建新版本”的产品约束，因此不能仅凭 URL 或公告 ID 永久跳过下载。

分两级处理：

1. 本轮实现内容寻址文件复用：下载后哈希相同则不重复落盘。
2. 后续根据真实响应头评估 ETag/Last-Modified 条件请求；只有服务器提供可靠校验器时才允许 304 复用解析结果。

不得使用固定时间窗口直接跳过公告，以免遗漏停发、延期、价格或申购日期变更。

### 3.5 PDF 阶段测试

新增或调整以下自动化覆盖：

- PDF/HTML 分块下载与哈希正确。
- 超出 Content-Length 上限时不读取正文。
- 未提供 Content-Length、读取过程中超限时安全终止。
- 临时文件在成功、取消、格式错误和超限后均正确处理。
- 相同内容只保留一个哈希文件。
- 同一 URL 内容变化仍创建两个版本。
- 伪 PDF/WAF HTML 不作为 PDF 保存。
- 页数和文本字符限制生效。
- 提前停止后关键字段和证据仍正确。
- 现有公告、同步、SQLite 和人工核验回归全部通过。

## 4. 阶段二：.NET 常驻内存收敛

PDF 阶段通过后，再按收益和风险排序实施：

1. 主窗口懒创建：后台启动只创建托盘与后台服务，首次打开时才构造 WPF 窗口。
2. 用 Win32 `Shell_NotifyIcon` 替换 WinForms `NotifyIcon` 和 `ContextMenuStrip`。
3. 评估取消 Windows App SDK：
   - 若托盘气泡、声音和置顶提醒窗口满足需求，则删除 Windows App SDK/Toast。
   - 若必须保留系统 Toast，则单独测量 Windows App SDK 框架依赖和自包含两种发布方式。
4. 仅在分配问题解决后评估 GC 内存保守配置；不把强制 `GC.Collect` 作为主要方案。
5. 增加可重复的内存测量脚本，输出 JSON/CSV 报告。

## 5. 阶段三：Rust/Slint 原型

### 5.1 原型范围

Rust 原型只证明架构、兼容性和资源占用，不直接替换正式版本。必须完成：

- Windows 单实例。
- 原生系统托盘与右键菜单。
- Slint 简单配置窗口：
  - 开机启动开关。
  - 三个市场开关。
  - 安全截止时间。
  - 立即同步。
  - 查看当天任务和运行状态。
- SQLite 读取现有数据库的只读兼容性验证。
- 定时同步骨架与取消机制。
- HTTP 流式下载和 SHA-256。
- 有界 PDF 处理接口；优先复用固定样本验证，不在第一版重写全部 PDF 解析规则。
- 提醒通道原型：托盘、声音、前台窗口；Toast 作为可选验证项。
- 退出和资源释放。

原型明确不包含：

- 自动下单或券商接口。
- 一次性完整移植全部诊断、安装、升级和恢复 smoke。
- 未经验证直接写入正式数据库。

### 5.2 Rust 目录与模块

建议目录：`prototypes/stock-ipo-reminder-rust/`

模块划分：

- `app`：生命周期、单实例和退出。
- `tray`：`windows-rs` 托盘和菜单。
- `ui`：Slint 配置/状态窗口。
- `storage`：SQLite 只读兼容层和原型设置库。
- `sync`：调度、HTTP 客户端和取消。
- `announcement`：流式下载、哈希和受限解析接口。
- `metrics`：Private Bytes、Working Set、句柄和线程采样。

### 5.3 Rust 验收

- Release 构建可在 Windows x64 无控制台运行。
- 托盘、Slint 窗口和退出流程稳定。
- 可以读取隔离副本中的现有设置和当天任务。
- 30 分钟空闲期间无持续句柄或线程增长。
- 使用与 .NET 相同的内存测量脚本和固定公告样本。
- 输出二进制/依赖体积、启动内存、同步峰值和回落值。

## 6. 迁移决策门槛

满足以下任一情况才进入正式 Rust 迁移设计：

1. .NET 完成 PDF、窗口和托盘优化后，空闲 Private Bytes 仍持续高于 100 MB。
2. Rust 原型在相同功能下至少降低 40% 空闲 Private Bytes，且没有牺牲通知可靠性、数据正确性和可诊断性。
3. Rust 原型能够稳定读取或迁移现有 SQLite 数据，并能复用固定响应样本建立等价回归。

若 .NET 已稳定低于目标，而 Rust 优势主要是安装体积，则保留 .NET 正式版本，Rust 原型作为后续版本储备。

## 7. 交付物

- 本方案文件。
- PDF 流式下载和有界解析实现。
- PDF/同步回归测试与内存测量报告。
- .NET 优化前后对比表。
- `windows-rs + Slint` Rust 原型。
- Rust 原型构建说明和内存/体积报告。
- 最终保留 .NET 或迁移 Rust 的建议。

## 8. 实施顺序与停止条件

1. 写入并评审本方案。
2. 完成 PDF 瘦身及测试。
3. 运行完整 .NET 回归和隔离内存测试。
4. 若 PDF 阶段出现数据正确性回归，停止后续阶段并先修复，不通过降低解析可靠性换内存。
5. 建立 Rust/Slint 原型。
6. 使用相同样本和脚本做最终对比。

## 9. 实施结果

### 9.1 PDF 与同步内存

已完成：

- 64 KiB 有界流式下载、增量 SHA-256、32 MiB PDF/4 MiB HTML 限制。
- 相同内容文件复用，成功、失败、取消和超限路径均清理临时文件。
- PDF 最多解析 20 页、256,000 字符，并在关键字段齐全时提前停止。
- PdfPig 移入同一可执行文件的短生命周期 `--pdf-worker` 模式；Worker 只返回文本哈希和字段 JSON，完成后立即退出。
- 自动同步改为启动时一次，之后每 24 小时一次；保留手动“立即同步”。
- 同步结束后执行 LOH 压缩、阻塞 GC 和 Windows Working Set trim。

隔离完整同步结果（27 个发行任务、6 份公告）：

| 指标 | 主进程内直接解析 | PDF Worker 后 |
| --- | ---: | ---: |
| 同步后 Private Bytes | 约 248 MB | 84.9 MB |
| 同步后 Working Set | 约 346 MB | 48.2 MB |
| 同步后托管存活堆 | 约 40.0 MB | 约 23.5 MB |
| 残留 Worker | 不适用 | 0 |
| 残留 `.tmp` 文件 | 0 | 0 |

结论：主进程稳定低于 100 MB 目标。同步期间允许 Worker 出现短时高内存，解析完成后由进程退出彻底归还。

### 9.2 Rust/Slint 历史原型阶段

当时的原型目录为 `prototypes/stock-ipo-reminder-rust/`；0.2.1 仓库整理后正式实现已迁移到根目录标准 Cargo 布局。

已实现：

- Slint 简单配置/状态窗口。
- `windows-rs` 原生托盘，支持双击打开、右键“打开设置/退出”。
- 自动启动、沪深北市场、安全截止时间和每日同步口径的配置界面。
- 现有 SQLite 的只读设置和当天任务统计。
- 原型配置写入独立 JSON，不修改正式数据库。
- `--background`、`--data-root`、`--exit-after-seconds` 测量参数。

Release 实测：

| 指标 | Rust/Slint 原型 |
| --- | ---: |
| 单个 EXE | 9.87 MiB |
| 可见配置窗口 Private Bytes | 6.7 MB |
| 可见配置窗口 Working Set | 24.7 MB |
| 空闲 40 秒线程/句柄 | 9 / 218，未持续增长 |

原型当前不接管正式网络同步、PDF 规则和通知可靠性链路；这些属于正式迁移阶段，而不是本轮轻量 UI/托盘可行性验证。

### 9.3 当前建议

该阶段建议已被后续实施取代：Rust 已补齐正式同步、PDF、提醒、SQLite 写入、诊断、安装升级和发布回归，因此从 0.2.0 起正式版本切换为 Rust。

## 10. Rust 0.2.0 正式迁移结果

### 10.1 正式技术栈与边界

- 唯一正式运行二进制：`StockIpoReminder.exe`，Cargo package 为 `stock-ipo-reminder`。
- UI：Slint；不使用 WinForms、WPF、Tauri 或 WebView。
- Windows 集成：`windows-rs`，覆盖托盘、气泡、声音、单实例、Working Set trim 和计划任务自启动。
- 网络与解析：Rust `reqwest`、`serde_json`、`lopdf`。
- 存储：Rust `rusqlite`，内嵌 SQLite，并兼容旧版数据库 schema 和设置 JSON。
- 正式安装包、便携包、自测试、smoke 和 audit 均不调用 `dotnet`，发布物不需要 .NET Runtime。
- 0.2.0 发布时旧 C#/.NET 项目仅作为历史基线保留，不进入正式构建和发布；0.2.1 仓库整理已将其删除。

0.2.1 起正式 Cargo 项目位于仓库根目录，使用 `src/`、`ui/`、`assets/` 和 `tests/fixtures/` 标准结构。

### 10.2 已迁移功能

- Slint 任务列表、详情、确认/撤销、人工字段覆盖、诊断导出和简单配置窗口。
- 窗口右上角关闭时隐藏到托盘；只有“安全退出”或托盘退出才结束进程。
- Eastmoney、SSE、CNINFO、BSE 四来源采集、JSONP/契约检查、来源失败隔离和指数退避。
- 近 30 天、Upcoming、Active 候选组过滤；同组无日期候选仍参与字段裁决。
- 多来源裁决、关键字段变更事件版本、旧确认失效和提醒重新规划。
- SQLite 事件、字段来源、版本、人工覆盖、Reminder Outbox claim/lease/complete/fail、来源健康、heartbeat 和健康摘要。
- SSE/CNINFO/BSE 正式公告检索、白名单校验、流式下载、哈希、字段解析和人工核验降级。
- 同一 EXE 的 `--pdf-worker-request/--pdf-worker-response` 短生命周期 PDF Worker。
- 数据备份、维护、滚动日志、脱敏诊断 ZIP、自测试、安装、升级前在线备份、失败回滚和普通卸载保留数据。

### 10.3 同步与内存模型

- 启动时自动同步一次，之后固定每 24 小时一次；手动同步始终可用。
- 同步阶段允许网络响应、候选和 PDF Worker 出现短时内存峰值。
- 每轮同步结束后不保留原始响应、候选或公告正文缓存，并在 Windows 上执行 Working Set trim。
- PDF Worker 完成或超时后退出；Worker 请求、响应、下载临时文件均清理。

2026-08-25 隔离真实联网验收：

| 指标 | Rust 0.2.0 |
| --- | ---: |
| 原始来源记录 | Eastmoney 500 / SSE 1474 / CNINFO 4 / BSE 347 |
| 近期候选 / 事件组 | 45 / 24 |
| 保存事件 / 公告 | 24 / 7 |
| 主进程同步峰值 Private Bytes | 13,701,120 bytes（约 13.1 MiB） |
| 主进程同步峰值 Working Set | 33,443,840 bytes（约 31.9 MiB） |
| 主进程 + 短生命周期 Worker 峰值 Private Bytes | 25,030,656 bytes（约 23.9 MiB） |
| 主进程 + 短生命周期 Worker 峰值 Working Set | 60,645,376 bytes（约 57.8 MiB） |
| 同步完成 5 秒后 Private Bytes | 13,701,120 bytes（约 13.1 MiB） |
| 同步完成 5 秒后 Working Set | 20,795,392 bytes（约 19.8 MiB） |
| 残留 PDF Worker | 0 |
| 残留 Worker/下载临时文件 | 0 |
| SQLite `integrity_check` | `ok` |

因此“同步期间可接受较高内存、同步后释放缓存、常驻低于 100 MB”的目标已满足。本次实测甚至没有接近 100 MB 门槛。

### 10.4 缺陷修复与回归

- 修复同步错误被静默吞掉的问题；失败会写入脱敏日志、设置 `last_error` 并恢复非同步状态。
- 修复新事件首次同步时先保存公告、后保存事件导致的 SQLite 外键失败；现在先保存事件和字段来源，再关联公告。
- 为公告外键写入顺序增加专门回归测试。
- 修复“确认已申购 → 撤销确认”后状态被重新写回已确认的问题；撤销、事件恢复和提醒重排改为同一事务。
- 修复 `--background` 重显窗口和关闭最后窗口导致托盘进程退出的问题；改用持续事件循环，仅安全退出才结束程序。
- EXE、Slint 标题栏和托盘统一使用正式多尺寸图标，并延迟到事件循环启动后安装标题栏图标。
- 0.2.1 修复托盘恢复时 Slint software renderer 偶发只绘制脏子区域的问题；所有显示入口统一标记整窗背景失效，并在显示后补一次全量重绘。
- Rust 固定 fixture 与存储测试共 16 项全部通过。

### 10.5 发布门禁

- `scripts/build-release.ps1`：只执行 Cargo 测试和 release 构建。
- `scripts/smoke-release.ps1`：验证便携版、自测试、空闲内存、安装、安装后自测试、卸载保留数据。
- `scripts/audit-release.ps1`：验证 manifest、哈希、包内容、Rust 实现标记、无 .NET 发布依赖和 Cargo 正式入口。
- release smoke 验证空闲 Private Bytes 5,095,424 bytes、Working Set 29,802,496 bytes，并通过便携、自测试、安装、安装后自测试和卸载保留数据门禁。
- release audit 验证 AMD64、Rust + Slint、单原生 EXE、无 .NET Runtime 文件及 Cargo 正式发布入口。
- 单 EXE 约 14.6 MiB，便携 ZIP 约 7.4 MiB。
- 正式发布包只包含 Rust EXE、README、RELEASE_NOTES、manifest 和校验文件，不包含 .NET DLL、`.deps.json` 或 `.runtimeconfig.json`。

2026-08-25 17:07（Asia/Shanghai）最终发布证据：

- Cargo 测试：16/16 通过；release 构建完成，仅保留 9 个非阻塞 dead-code 警告。
- 便携包：`artifacts/release/0.2.0/StockIpoReminder-0.2.0-win-x64-portable.zip`，7,727,892 bytes，SHA-256 `369311d5631ca6fff51bb70d964199efe888e0bb7db7c6e57633416154c75fef`。
- 安装包：`artifacts/release/0.2.0/StockIpoReminder-Setup-0.2.0-win-x64.exe`，15,326,208 bytes，SHA-256 `8aa91fb45c6544307d3f365a69d29545906914881b5ac3d6505c0c9410dbfaa9`。
- 最终 smoke：`artifacts/smoke/windows-rust-0.2.0-20260825-170727.json`，全部门禁通过；空闲 Private Bytes 5,033,984，Working Set 27,664,384。
- 最终 audit：`artifacts/audit/release-rust-0.2.0-20260825-170747.json`，6 项审计全部通过。
- 正式 EXE 联网同步内存报告：`artifacts/memory/rust-0.2.0-20260825-170416.json`；四来源成功、24 个事件、7 份 PDF 公告、同步后 Worker/临时文件残留为 0。
- 正式 EXE 图标可由 Windows 提取，关联图标非空（32×32）；Slint 标题栏和系统托盘已通过真实 UI 验收。

仍需单独补充的发布证据只有 Windows 10 2004+ 实机 smoke 和 Authenticode 签名；它们不影响“代码与发布链已经完全迁移到 Rust”的结论。

### 10.6 Rust 0.2.1 托盘恢复修复发布证据

2026-08-25 23:22（Asia/Shanghai）完成补丁版本与纯 Rust 仓库整理后的最终发布：

- 版本号已从 0.2.0 升级为 0.2.1；Cargo、默认发布脚本、README、发布说明和运行时 User-Agent 已同步。
- 托盘隐藏后恢复统一执行整窗背景失效，并在原生窗口显示完成 50 ms 后补一次全量重绘；覆盖托盘双击、托盘设置、通知点击、提醒弹窗和退出确认。
- 用户实机验证隐藏与托盘双击恢复正常；Rust 固定 fixture 与存储回归测试 16/16 通过。
- 便携包：`artifacts/release/0.2.1/StockIpoReminder-0.2.1-win-x64-portable.zip`，7,727,935 bytes，SHA-256 `31dcc7641c2af57ab7c41c12e69b7fc9f589c730d8d07dd424e2f4b7c858e5e5`。
- 安装包：`artifacts/release/0.2.1/StockIpoReminder-Setup-0.2.1-win-x64.exe`，15,326,720 bytes，SHA-256 `623a67120878e79957abcfca4e4e2220b6f634dff061af38f835c75bdcf08d53`。
- smoke：`artifacts/smoke/windows-rust-0.2.1-20260825-230830.json`，便携、自检、后台 UI、安装、安装后自检、卸载保留数据和低于 100 MB 门禁全部通过；空闲 Private Bytes 4,882,432，Working Set 27,664,384。
- audit：`artifacts/audit/release-rust-0.2.1-20260825-232231.json`，7 项发布审计全部通过，并新增活动源码树无 C#/WPF/project/solution 文件门禁。
- 联网同步内存：`artifacts/memory/rust-0.2.1-20260825-232125.json`，同步完成；主进程峰值 Private Bytes 13,615,104，进程族峰值 26,226,688；同步后 Private Bytes 13,615,104 / Working Set 20,951,040，短生命周期 PDF Worker 峰值 1，完成后 Worker 和临时文件残留均为 0。

随后完成仓库最终整理：正式 Rust crate 从历史 `prototypes` 路径迁移到根目录；固定响应样本和图标分别迁移到 `tests/fixtures` 与 `assets`；旧 C#/.NET/WPF 源码、测试、工具、本地 SDK、bin/obj/target 和旧发布临时证据全部删除。整理后仓库不存在 `.cs`、`.xaml`、`.csproj` 或 `.sln` 文件，构建与运行均不依赖 .NET。

### 10.7 Rust 0.2.2 最终发布证据

2026-08-25 23:30（Asia/Shanghai）完成最终版本号更新与全量发布验收：

- Cargo、运行时 User-Agent、四个发布/验收脚本、README 和发布说明已统一为 0.2.2；Rust 回归测试 16/16 通过。
- 便携包：`artifacts/release/0.2.2/StockIpoReminder-0.2.2-win-x64-portable.zip`，7,729,631 bytes，SHA-256 `0d15010b6b237d02481631a9c921112b1a3e1c2d4f30fb90b72ffbd7b0583521`。
- 安装包：`artifacts/release/0.2.2/StockIpoReminder-Setup-0.2.2-win-x64.exe`，15,326,720 bytes，SHA-256 `81c205d9a9f1ed42ffae255d484f34615078cd57ddd29b2421a967125af64bc0`。
- smoke：`artifacts/smoke/windows-rust-0.2.2-20260825-232940.json`，便携、自检、后台 UI、安装、安装后自检、卸载保留数据和低于 100 MB 门禁全部通过；空闲 Private Bytes 4,902,912，Working Set 27,660,288。
- audit：`artifacts/audit/release-rust-0.2.2-20260825-232956.json`，7 项审计全部通过；包括单 Rust EXE、AMD64 PE、Cargo 发布入口和 `source.rust-only` 活动源码树门禁。
- 联网同步内存：`artifacts/memory/rust-0.2.2-20260825-233008.json`，46 条近期候选、25 个事件、7 份公告、四来源成功且零失败；主进程峰值 Private Bytes 20,475,904，进程族峰值 28,610,560；同步后 Private Bytes 13,582,336 / Working Set 22,097,920，短生命周期 PDF Worker 峰值 1，完成后 Worker 和临时文件残留均为 0。
- 最终发布物未进行 Authenticode 签名；`release-manifest.json` 明确记录 `signed: false`。Windows 10 2004+ 的正式支持声明仍需对应系统实机 smoke 证据。
