# 主窗口首次打开后下方整条"透明"（未绘制）问题

- 状态：已按 P1～P3 实施，P4 调整为「检测 + 日志」并随 `0.4.3` 发布（详见下方「实施结果」）。
- 现象：托盘/二次启动唤起主窗口后，窗口下边缘出现一条贯穿整宽的"透明"区域，能直接看到桌面图标与任务栏；手动拉伸窗口后恢复正常。
- 复现环境：本机 3840x2160 @150%（DPI 144，工作区 3840x2088），安装版 0.4.2（`C:\Program Files\StockIpoReminder\StockIpoReminder.exe`）。
- 复现方式：用临时数据目录 + 用户当前 `window-state.json`（1499x1128）启动，再广播 `StockIpoReminder.Activate.<hash>` 唤醒（等价于托盘点击）。

## 结论（已用诊断日志 + 像素测量验证）

首次显示时窗口"先显示、后改尺寸"，而工作区钳制/居中用的是 **Slint 缓存的旧尺寸**，导致窗口底边落到屏幕下边缘之外；软件渲染器只把"当时屏幕内"的像素写进窗口表面，**屏幕外那一段从未被绘制**，之后也没有任何一次整窗重绘去补它，于是桌面从这条区域透出来。

### 证据 1：临时诊断日志（已还原）

```
show+20ms  slintPhysical=1200x750 scale=1.5 outer=1210x784 at 608,448 client=1200x750 root=[h=500;w=800;]
恢复尺寸前(1499x1128) slintPhysical=1200x750 ... client=1200x750
恢复尺寸后        slintPhysical=1200x750 ... client=2249x1692   <- 原生窗口已变大，Slint 缓存仍是旧值
工作区调整：无变化 current=1200x750 available=3806x2030          <- 用旧尺寸做钳制/居中，钳制失效
show+60ms  slintPhysical=2249x1692 ... root=[... h=1128; w=1499.33]<- Slint 才追上真实尺寸
```

- 窗口由 Slint 以 **最小尺寸** 800x500 逻辑像素（=1200x750 物理）创建；
- `apply_restored_main_window_size` 请求 1499x1128 逻辑（=2249x1692 物理）后，原生窗口立刻变大，但 Slint `window.size()` 仍是 1200x750；
- 紧接着执行的 `fit_window_to_work_area` 用这个旧值算出 `target=1200x750` → "无变化"，既没有钳制也没有按真实尺寸居中；
- 最终窗口落在 (1315,652)、外框 2262x1728，底边 = 2380 > 屏幕高 2160，**下沿 220px 在屏幕外**。

### 证据 2：像素级透明区域测量

对"窗口显示中"与"应用自身隐藏后（桌面）"同一屏幕区域逐像素比较：

```
rows identical to desktop (fully transparent): first=1436  count=232
last row that differs from desktop: 1501
row 1521/1561/1621: identical-samples 133/133
```

- 窗口高 1728（外框），**第 1436~1667 行与桌面完全一致** → 这段就是未绘制的透明区；
- 1436 ≈ 2160 − 652 − 边框，即"屏幕内 / 屏幕外"的分界，与证据 1 完全吻合。

### 证据 3：为什么"拉伸后才正常"

- 软件渲染走 `softbuffer`，每次只 `BitBlt` Slint 报告的脏矩形；`present_with_damage` 只提交脏区包围盒；
- 尺寸未变化时（例如移动窗口、屏幕新暴露区域），Slint 的增量脏区为空 → 直接不提交，未绘制区域永远不会被补上；
- 一旦发生 **尺寸变化**，`softbuffer` 重新分配缓冲 → Slint 走 `RepaintBufferType::NewBuffer` 全量重绘 → 整窗重绘，透明条带消失。实测把窗口尺寸 ±2px 后，底部区域立即恢复正常。

## 附带发现（独立缺陷，建议同批修）

- 用户机器上 `GetDpiForWindow(主窗口) = 96`，而显示器是 144（150%）：窗口在**隐藏期间**经历了显示缩放变化，缓存的 DPI 没有更新，`winit/Slint` 认为缩放系数是 1.0（窗口非客户区也按 100% 绘制，标题栏高度 24px vs 正常 34px）。
- 这解释了两件事：保存下来的历史尺寸（如 1083x1371、1168x1364）只有在 1.0 缩放下才物理可行；且"拉伸"只修好了绘制，没有修好缩放（拉伸后 DPI 仍是 96）。
- 影响：UI 实际按 1.0 渲染（在 150% 屏上偏小）、逻辑尺寸与物理尺寸差 1.5 倍，任何按逻辑尺寸做的窗口/布局换算都会失真。

## 修复方案

### P1 首帧就定好尺寸（核心）

`src/ui/runtime_bridge.rs` 的 `show_and_repaint`：在 `show()` **之前** 把待恢复尺寸（`apply_restored_main_window_size` 里的 `PENDING_MAIN_WINDOW_SIZE`）通过 `window.window().set_size(LogicalSize)` 下发。此时原生窗口尚未创建，winit 会把尺寸写进窗口属性，窗口"一出生"就是目标尺寸，不再出现"先小后大"。

- 无保存状态时建议同时以 `preferred-width/height`（1180x780）兜底，避免首帧停在最小尺寸 800x500（当前行为）。
- `apply_restored_main_window_size` 保留为"窗口已存在时的兜底路径"，语义改为幂等（尺寸一致时不下发）。

### P2 钳制改用真实几何

`src/windows_integration/window.rs` 的 `fit_window_to_work_area`：现在用 `window.size()`（Slint 缓存，可能滞后一帧）算 `current_size`；改用本来就已读取的 `GetClientRect` 结果作为 current，让钳制与居中永远基于原生窗口的真实尺寸，不再依赖缓存时序。

### P3 最后一次尺寸变更之后保证整窗重绘

把 `show_and_repaint` 里固定 50ms 的 `force_full_repaint` 改成"落定后重绘"：短轮询（最多 3 次、每次约 120ms）直到原生客户区尺寸与目标一致，再调用一次 `force_full_repaint`；随后仍保留一次延迟加固重绘（现有 50ms 那次保留即可）。这样即使 P1/P2 之外还有别的尺寸变化来源（DPI 变化、用户拖拽、系统贴靠），也不会留下未绘制区域。

### P4 DPI 陈旧补偿（可选，建议一并做）

在 `show_and_repaint` 的原生准备阶段比较 `GetDpiForWindow(hwnd)` 与 `window.scale_factor()`：

- 不一致时按真实 DPI 重新换算恢复尺寸（把逻辑目标乘 `real_dpi / slint_scale / 96`）后再 `set_size`，至少保证像素尺寸正确；
- 若要彻底修正 UI 缩放，需要在同一处触发一次窗口 DPI 重算（例如临时改变窗口尺寸/位置使系统重发 `WM_DPICHANGED`，或在日志中记录并在后续版本改为启动时检测），本计划先做"补偿 + 记录日志"，避免引入不确定行为。

### P5 验证与回归

- 手工：用 `--data-root <临时目录>` + 预置 `window-state.json`（大于默认尺寸、且接近工作区高度）启动 → 广播激活消息唤出窗口 → 截图检查窗口底边在工作区内、且窗口底部的像素与窗口背景一致（不等于桌面）。
- 交互：托盘点击、二次启动唤醒、任务栏贴靠/还原、多显示器与 100%/150% 缩放切换各走一遍；确认 `window-state.json` 保存/恢复仍正确，`MAIN_WINDOW_NATIVE_PREPARED` 单次语义不变。
- 单元测试：`clamp_window_dimension` 相关测试保持通过（`rtk cargo test`），并为"钳制使用真实几何"补一个纯函数级用例（给 current/available/期望 target 的表驱动断言）。
- 发布：按仓库约定同步 `CHANGELOG.md`（`[未发布]`）与 `RELEASE_NOTES.md`，并重新生成安装包与校验文件。

## 风险

- P1 依赖 winit "窗口未创建时把 inner size 存进属性"的行为（`i-slint-backend-winit` 的 `resize_window` 已显式实现该分支），若某次 Slint 升级改变该行为，首帧尺寸会退回旧路径——P3 的落定后重绘仍能兜住绘制正确性。
- P2 改动只影响"首帧后会立即再被修正"的计算路径，不改动画布尺寸策略。
- P4 属独立缺陷，需单独回归 100%/150% 两种缩放下的窗口尺寸与文字清晰度。

## 实施结果（0.4.3）

- P1：新增 `ui::window_state::apply_initial_main_window_size`，在 `show_and_repaint` 的 `show()` 之前下发尺寸（进程内只生效一次）；待恢复尺寸为空时改用 `ui/main.slint` 的首选尺寸 1180x780。日志事件 `event=main_window_size_preapplied`。
- P2：`windows_integration::fit_window_to_work_area` 的 `current_size` 改为原生客户区几何（新增 `window_client_size`），钳制与居中的 `frame_width/frame_height` 也来自同一次 `GetWindowRect`/`GetClientRect`。
- P3：`apply_restored_main_window_size` 幂等化（尺寸已等于保存值时立即记 `event=main_window_size_restored`，不再发起多余改尺寸；需要补正时仍在 150ms 后记同一条事件），新增 `runtime_bridge::schedule_settled_repaint`：每 120ms 轮询一次、最多 8 次，等到 `window_size_settled`（Slint 缓存尺寸 == 原生客户区）或次数用尽后整窗重绘一次。
- P4 调整：不做尺寸换算补偿。原因是那样只会让窗口变大、而 Slint 仍按旧缩放渲染，反而造成"窗口尺寸与界面字号错配"；改为新增 `windows_integration::window_dpi_mismatch`（`GetDpiForWindow` 对比显示器 DPI）并在首次显示时写一条 WARN（`event=main_window_dpi_stale`，提示重启程序恢复）。同时接受现状：本机实测该窗口仍是 96 DPI、显示器 144 DPI，重启程序即恢复。
- P5：`rtk cargo fmt` / `rtk cargo test`（175 项）通过；复现脚本对"历史保存尺寸 1499x1128""保存尺寸大于工作区 2600x1500""全新数据目录"三种场景逐像素验证，修复后整行未绘制区域均为 0；未覆盖多显示器、缩放切换与任务栏贴靠的手工走查。
