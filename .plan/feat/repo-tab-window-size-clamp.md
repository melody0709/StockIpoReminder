# 「逆回购」页签导致主窗口缩放失效 — 研究与修复方案

- 日期：2026-11-03
- 版本：0.5.0（84f51a7 引入逆回购/信用卡页签后的行为）
- 结论：**Bug，不是有意为之。** 应用代码里没有任何"按页面改窗口尺寸"的逻辑；问题出在逆回购页的布局约束泄漏到了窗口级，触发 Slint winit 后端的"强制满足约束"机制，把窗口钳进一个偏小的区间并禁止拉宽。

## 1. 现象

1. 用户手动把主窗口调大后，「今日任务 / 未来 60 天 / 信用卡 / 健康与诊断 / 设置」都保持用户尺寸；
2. 一点「逆回购」页签，窗口立刻缩回一个较小尺寸；
3. 在逆回购页上无法再把窗口拉大（拖拽无效），表现为"被限制到某一尺寸"；
4. 切到其它页后，被缩小的尺寸已经生效（且可能已被持久化覆盖，见 §5）。

## 2. 根因链

Slint 会把**组件根布局的 min/max 约束**当作操作系统级窗口尺寸约束，并在约束变化时**强制把窗口改到约束区间内**。逆回购页恰好产生了一个有界的 max-width，其它页面没有。

逐环节（带证据）：

1. **固定 `width` = 同时钉死 min 和 max。**
   逆回购页「提醒时段」表格行（[main.slint](file:///d:/GITHUB_melody0709/stock_new/ui/main.slint#L898-L906)，修复后行号）的 4 个控件全部用 `width:` 固定宽（Toggle 174px ×2、LineEdit 104px ×2），行内**没有一个可拉伸/无界兄弟元素**。
   Slint 编译器把显式 `width` 同时写入 min-width 与 max-width（`i-slint-compiler-1.17.1/layout.rs:252-265`，`fixed_width=true`），因此该 HorizontalBox 的横向约束是 min=max=586px（174+104+174+104+3×10 spacing）。

2. **VerticalBox 按 `max = min(各子元素 max)` 聚合横向约束。**
   `LayoutInfo::merge`（`i-slint-core-1.17.1/layout.rs:55-64`）取 `max: self.max.min(other.max)`。于是 586px 这个有界 max 向上汇入「提醒时段」卡片（+卡片内边距 28px ≈ 614px）、逆回购整页 VerticalBox，最终把**整列 max-width 钉死在约 654px**（含根 VerticalBox 左右 padding 20×2）。

3. **约束汇总到窗口级 OS 约束。**
   组件根 `layout_info` 由布局合成的 `layoutinfo-*` 属性与显式约束覆盖生成（`i-slint-compiler-1.17.1/llr/lower_layout_expression.rs:1477-1548`），经 `WindowProperties::layout_constraints`（`i-slint-core-1.17.1/window.rs:345-359`）交给后端。
   [main.slint](file:///d:/GITHUB_melody0709/stock_new/ui/main.slint#L274-L277) 的 `min-width: 800px` 是显式约束、覆盖布局 min；但 **max 没有显式设置**，保留了内容推出来的 ≈654px。
   再经 `min_max_size_for_layout_constraints` 的交叉修正（`i-slint-core-1.17.1/layout.rs:86-89`：`min.min(max)` / `max.max(min)`），窗口生效区间变成 **宽 [654, 800] 逻辑px**。

4. **切页 → 约束变化 → 后端强制改窗口尺寸。**
   `if root.active-page == 4 : VerticalBox` 出现/消失使约束改变，`WindowPropertiesTracker` 触发 `update_window_properties`（`i-slint-core-1.17.1/window.rs:387-396`）→ `apply_constraints`（`i-slint-backend-winit-1.17.1/winitwindowadapter.rs:1407-1428`）调用 `set_min_inner_size` / `set_max_inner_size`，并执行 `adjust_window_size_to_satisfy_constraints`（同文件 1688-1722 行，注释原文："Winit doesn't automatically resize the window to satisfy constraints. Qt does it though, and so do we here."）——**把当前尺寸硬钳进 [min, max]**：
   - 窗口宽 > 800 → 立刻被改小（现象 2）；
   - `set_max_inner_size` 生效后拖拽无法超过 800（现象 3）；
   - 若 min==max 还会 `set_resizable(false)`（`window_is_resizable`，同文件 149-161 行）。

5. **为什么只有逆回购页出事——对照组验证。**
   - 信用卡页表单行（[main.slint](file:///d:/GITHUB_melody0709/stock_new/ui/main.slint#L975-L985)）：固定宽 LineEdit（96/108/132px）与**无 max 的 Text 标签**混排在同一 HorizontalBox → 行 max = Σ max = ∞，不钉死列宽。
   - 设置页节能同步行（[main.slint](file:///d:/GITHUB_melody0709/stock_new/ui/main.slint#L1396-L1411)）：同样有无界 Text 兄弟（"默认 30 分钟"等）。
   - 逆回购行（902-905）是全仓库**唯一**一行 4 个子元素全部固定宽、没有无界兄弟的横排，因此是唯一产生有界 max 的页面。

## 3. 附带伤害：用户尺寸被持久化覆盖

[window_state.rs](file:///d:/GITHUB_melody0709/stock_new/src/ui/window_state.rs#L150-L196) 的尺寸记忆以 500ms 观察 + 1s 防抖把当前尺寸写入 `window-state.json`。用户进入逆回购页停留约 1.5 秒后，**被钳制的尺寸会覆盖掉用户手动调整的尺寸**，即使切走页面也回不来——这解释了"缩了之后就定住了"的体感。

## 4. 修复方案

### 方案 1（推荐）：给 MainWindow 显式设置 max-width / max-height

在 [main.slint](file:///d:/GITHUB_melody0709/stock_new/ui/main.slint#L274-L277) 的 `MainWindow` 上补：

```slint
max-width: 10000px;
max-height: 10000px;
```

依据：根元素的显式约束在 `get_layout_info` 中**直接覆盖**布局汇总出的同名字段（`lower_layout_expression.rs:1514-1544`，`for_each_restrictions` 按字段名替换），与现有 `min-width: 800px` 覆盖 min 是同一机制。10000 与 [window_state.rs](file:///d:/GITHUB_melody0709/stock_new/src/ui/window_state.rs#L24) 的 `MAX_LOGICAL_DIMENSION` 保持一致。

- 优点：一处改动，根治当前与**未来任何页面**的同类泄漏；不改变任何页面内部布局；min 侧仍由 800×500 保护。
- 风险：极宽窗口下逆回购的固定宽控件行不会随窗口变宽（本来如此，行为不变）。

### 方案 2（内容层修复，不推荐单独使用）

把 902-905 的 `width:` 改成不产生 max 约束的写法（如 `min-width` + `horizontal-stretch: 0`）。问题：Slint 布局内"固定宽度"必然产生有界 max（width = min+max），而 `horizontal-stretch: 0` 对 Toggle 曾被验证不生效（CHANGELOG 0.5.0 已记录），改写有回归风险；且只要将来再出现"整行全固定宽"就会复发。可作为方案 1 之后的可选清理，不承担修复职责。

### 方案 3（可选加固）：持久化防覆盖

在 `start_main_window_size_persistence` 中跳过"尺寸等于 OS min/max 钳制结果"的观察值，防止历史版本已经把钳制值写进 `window-state.json` 后用户每次启动都被锁小。方案 1 生效后此问题自然消失，仅在需要兼容已污染的 `window-state.json` 时补做（或一次性让钳制值不落盘即可）。

## 5. 验证计划

1. `rtk cargo fmt` + `rtk cargo test`（Rust 侧无逻辑改动，预期全绿）。
2. 手动回归：
   - 主窗口调大到 > 800 逻辑宽，依次切换 6 个页签，窗口尺寸均不变；
   - 逆回购页可继续拖拽放大/缩小，边界仍受 800×500 min 保护；
   - 逆回购页停留 5 秒后 `window-state.json` 记录的是用户尺寸；
   - 重启后尺寸恢复为用户调整后的值。
3. `scripts/smoke-release.ps1` 的 `mainWindowSizePersistence` 断言继续有效。
4. 按 AGENTS.md 走 `build.bat --package` + `validate-build-layout.ps1` 出包。

## 6. 结论摘要

| 问题 | 答案 |
| --- | --- |
| 有意为之还是 Bug？ | Bug。没有业务代码要求逆回购页固定窗口尺寸 |
| 根因 | 逆回购「提醒时段」整行全固定宽（`width: 174/104px` ×4）→ 页面 max-width 被钉死 ≈654px → Slint 后端把窗口钳进 [654,800] 并禁止拉宽 |
| 为什么其它页没事 | 信用卡/设置的固定宽控件行都有无界 Text 兄弟，行 max 为 ∞，不产生钳制 |
| 推荐修复 | MainWindow 加 `max-width: 10000px; max-height: 10000px;`（显式覆盖布局汇总的 max） |
| 附带伤害 | 停留 1.5s 后被钳制尺寸会覆盖 `window-state.json` 里的用户尺寸 |

## 7. 实施与验证记录（2026-11-03）

**实施**：采用方案 1。[main.slint](file:///d:/GITHUB_melody0709/stock_new/ui/main.slint#L283-L284) 的 `MainWindow` 增加 `max-width: 10000px; max-height: 10000px;`（附注释说明根因）；`CHANGELOG.md` `[未发布]` 与 `RELEASE_NOTES.md` 已同步登记。逆回购行的固定宽布局未动，0.5.0 的「输入框被顶飞」修复无回归。

**深度审查补充确认**：
- `builtins.slint:1571` 明确 Window 的 `max-width` 由窗口管理器执行，语义符合预期；
- `gen_layout_info_prop`（`default_geometry.rs:163-210`）跳过带 `x/y` 定位的条件子元素，确认弹窗矩形不参与约束聚合，无泄漏；
- 显式约束按字段名覆盖布局汇总值（`lower_layout_expression.rs:1514-1544`），即使布局合成值带 654 的 max 也会被 `max-width: 10000px` 覆盖。

**GUI 实测**（隔离数据目录 + 新构建 EXE，Win32 `SetWindowPos` 模拟用户拉伸，读 `GetWindowRect` 比对）：

| 步骤 | 窗口尺寸（物理像素） | 结果 |
| --- | --- | --- |
| 拉大窗口后停在「今日任务」 | 1800×1100 | 基准 |
| 切到「逆回购」页 | **1800×1100（不变）** | 修复前会被钳到 ≤800 逻辑宽，PASS |
| 在「逆回购」页继续拉大 | 1900×1150（成功） | 修复前会被 max 挡住，PASS |

**补充复核（2026-11-03 复审，隔离数据目录 + 150% DPI，Win32 `SetWindowPos` 模拟拉伸）**：
- 「信用卡」页缺口已补：1600×1000 下切页后窗口仍为 1600×1000。
- 「逆回购」页在停留状态下继续拉大到 1750×1100 成功，即「拉不回来」已消失。
- 两页截图确认内容铺满窗口宽度，无文字裁切、控件重叠或错位。
- 回归护栏：`src/ui/tests.rs` 新增 `main_window_declares_window_level_max_size`，断言 `MainWindow` 仍声明窗口级 `max-width/max-height`，防止这两行被删掉后问题复发。

**仍未覆盖**：多显示器、运行时切换缩放、窗口最大化到 4K 全屏，按 §5 人工走查。
