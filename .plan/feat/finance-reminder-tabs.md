# 逆回购与信用卡还款提醒 Tab 方案

- 状态：已实施，v0.5.0 打包与发布冒烟已通过
- 记录日期：2026-10-31（北京时间）
- 版本基线：`Cargo.toml` 当前 `0.5.0`，SQLite schema 为 v12
- 范围：只新增「逆回购」与「信用卡」两个顶层 Tab、对应本地存储和通知调度；不改动 IPO 业务规则，不接入券商或银行账户
- 一句话结论：两个功能共用「交易所日历 + 纯函数计划器 + 已展示台账 + 通用提醒协调器」，但业务规则、文案和确认语义完全分开；逆回购默认 **14:50 主提醒**，可选 **15:20 收盘前兜底**，信用卡按「每月还款日 + 提前 N 个自然日」生成可去重的本地提醒。
- 评审修订（2026-10-31）：总开关/卡片暂停恢复的 off→on 会刷新 `rules_changed_at`，不补已过时点；信用卡逾期分支同样受该时间约束，并扫描上月以覆盖跨月逾期；提醒 key 加入 `due_on`，修正还款日后同账期剩余档位可重新提醒；v1 为串行队列，不做跨类型正文合并，但 IPO 永不排队、可直接抢占 finance；卡逾期提醒时间沿用卡片设置；撤销「今日已处理」只恢复尚未展示的后续时点；自定义时间仅覆盖下午时段。

---

## 1. 需求拆解与产品结论

### 1.1 逆回购

用户目标不是获取行情，而是在容易忘记的时点提醒处理闲置资金，避免资金在账户里无收益过夜。

产品结论：

1. 只在沪深交易所**真实交易日**提醒。不能用「周一至周五」代替，也不能把国务院调休上班日当作交易日。
2. 默认启用 **14:50 主提醒**，定位为「A 股尾盘、偏利率的提前提醒」，给用户留出比较价格和下单的时间。
3. 提供可选的 **15:20 兜底提醒**，定位为「逆回购最后 10 分钟」。交易所逆回购可交易至 15:30，15:20 是最后处理窗口，不是 14:50 的替代。
4. 界面必须写清：14:50 只是通常更从容的提前时点，**不保证利率一定更高**；利率随资金面实时波动。
5. 不抓取实时利率，不显示臆测收益，不做自动下单。第一版是纯本地时点提醒。
6. 每个时点各有 10 分钟补发窗口：14:50 的窗口到 15:00，15:20 的窗口到 15:30。15:30 后一律不补发。应用在窗口内启动或从休眠恢复时补发一次，窗口外视为过期。
7. 若用户在 14:50 弹窗点击「今日已处理」，取消当天的 15:20 兜底，避免重复打扰。该操作只表示「不再提醒」，不声称已成交。
8. 交易日历未覆盖目标日期时 **fail-closed**：不提醒，并在 Tab 内明确显示「日历未覆盖，请更新版本」，绝不退回「工作日」猜测。

### 1.2 信用卡

用户目标是避免忘记还款，而不是管理账单或记账。

产品结论：

1. 每张卡只要求：别名、每月还款日（1–31）、提醒时间、提前提醒天数。银行名称可选。
2. 不采集完整卡号、CVV、额度、账单金额；连卡号后四位也不建议采集。需要区分多张卡时用「招行 Visa」「工行运通」等别名。
3. 默认提前 **3 天 + 还款日当天** 各提醒一次；可选 7/3/1/0 天，最多选择 4 个档位。
4. 还款日按**自然日**计算，不顺延周末和法定节假日；银行合同日的顺延规则不由本程序猜测。
5. 还款日为 29/30/31 而当月没有该日时，钳制到当月最后一天，并在编辑页实时预览下一期日期。
6. 到期未标记已还时，次日按该卡的同一提醒时间只补一次逾期提醒；之后界面持续显示逾期天数，不每日轰炸。
7. 「本期已还」只更新用户自己的本地状态，用于停止本期提醒和推进账期，不代表银行入账结果。
8. 所有数据只存本机 SQLite；不进入 IPO 第二通知通道，不写入日志、诊断包或远程请求。

---

## 2. 现状核查（代码证据）

### 2.1 导航与 UI

- 顶层页面是 `active-page` 整数，现为 0 今日、1 未来 60 天、2 健康、3 设置：`ui/main.slint:267`。
- 导航按钮：`ui/main.slint:550-560`；各页面条件渲染：`ui/main.slint:606`、`:704`、`:789`、`:873`。
- `NavButton` 非紧凑最小宽度 128px，紧凑 100px：`ui/main.slint:107-126`。
- 当前窗口最小宽度 800px：`ui/main.slint:256`；`compact-width` 阈值 1000px：`ui/main.slint:273`。
- 6 个 Tab 在 800px 紧凑宽度下为 `6×100 + 5×6 + 左右 padding 24 ≈ 654px`，可容纳；1000px 非紧凑下约 `6×128 + 30 + 40 ≈ 838px`，也可容纳。真正需要处理的是同一行的健康摘要文本，必要时在 `< 1120px` 隐藏，而不是压缩 Tab。
- 专用 `ReminderWindow`、Toast、托盘回退存在，但窗口按钮和批量逻辑默认 IPO 事件：`ui/main.slint:1779-1856`、`src/ui/runtime_bridge.rs:526-600`。
- Toast 激活回调把参数一律当股票 ID 打开详情：`src/native_tray.rs:192-207`。

### 2.2 时间与调度

- 固定北京时间 UTC+8：`src/core.rs:8-18`；不依赖 Windows 当前时区。
- 后台按绝对 deadline 休眠，每分钟最多心跳一次：`src/runtime/scheduler.rs:21-44`、`src/runtime/run_loop.rs:148-218`、`src/runtime/mod.rs:39`。
- 目前唯一「工作日」判断是 `!Saturday && !Sunday`：`src/runtime/scheduler.rs:294-296`。
- 没有交易所节假日表、日历覆盖年份、日历过期策略，也没有 `chrono-tz` 等依赖：`Cargo.toml:11-28`。
- 休眠恢复、解锁、系统时间变化会触发 Runtime recovery，5 秒防抖：`src/native_tray.rs:707-752`。

### 2.3 提醒 Outbox 与通知

- `reminder_outbox.ipo_event_id NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE`，强绑定 IPO：`src/storage/schema.rs:405`。
- 投递对象 `ReminderDelivery` 必须携带完整 `IpoEvent`：`src/model.rs:269-288`。
- 领取时 JOIN `ipo_events` 并构造股票投递：`src/storage/reminders.rs:83-118`。
- 第二通道文案固定「新股提醒」并附券商核对声明：`src/secondary_notification.rs:381-410`。
- 当前显示成功即完成 delivery，不等于业务已处理：`src/ui/runtime_bridge.rs:265-311`。

结论：不能伪造 IPO 事件承载逆回购/信用卡，否则会污染今日列表、健康计数、同步裁决、确认语义和远程通知。应新增**独立「已展示台账 + 纯函数计划器」**，只复用运行时调度与呈现机制。

### 2.4 设置与安全边界

- `AppSettings` 使用 serde default，新增字段可兼容旧 JSON：`src/model.rs:173-241`。
- 设置实际存放在 SQLite `app_settings` 单行 JSON，保存时会重排 IPO 提醒：`src/storage/settings.rs:20-56`。
- 「恢复默认设置」不应清空信用卡数据，因此信用卡和逆回购配置不放入 `AppSettings`。
- 诊断摘要可能包含完整 `AppSettings`：`src/operations.rs:309-415`；信用卡数据必须排除在诊断包外。
- 当前数据库备份/指纹逻辑需要随 v12 表同步：`src/storage/maintenance.rs:99-159`。
- 发布载荷是单 EXE，布局使用 allowlist：`scripts/validate-build-layout.ps1:117-188`。日历应 `include_str!` 编译进 EXE，不增加旁挂文件。

---

## 3. 外部规则与开源项目调研

### 3.1 逆回购交易时间：以现行规则为准

上交所、深交所「债券通用质押式回购（俗称国债逆回购）」采用匹配成交时：

```text
开盘集合匹配：09:15–09:25
连续匹配：    09:30–11:30、13:00–15:30
```

依据：

- 深交所《债券交易业务指南第 4 号（2025 年 1 月修订）》明确「连续匹配 09:30–11:30；13:00–15:30」，无单独收盘集合竞价：[深交所 2025 指南](https://docs.static.szse.cn/www/lawrules/service/bond/W020250113636009196077.docx)
- 深交所固定收益信息平台现行页面同样写「连续匹配 9:30-11:30，13:00-15:30」：[深交所回购交易](https://szse.cn/bond/introduction/traderule/repotrade/)
- 上交所《债券交易规则》将 15:30 作为收盘价计算边界：[上交所债券交易规则通知](https://www.sse.com.cn/lawandrules/sselawsrules2025/bond/trading/currency/c/c_20250604_10780834.shtml)

**重要纠偏**：网上仍大量引用 2019 年深交所「15:27–15:30 收盘集合竞价」的旧通知。该细节已被 2022 年后的债券交易规则体系取代，本方案不采用 15:27 限制，也不把它写进 UI。

因此：

- 「最后 10 分钟」= 15:20–15:30，提醒放在 **15:20** 合理。
- 14:50 是 A 股尾盘时点，不是逆回购收盘时点。它作为默认主提醒有产品价值，但必须与「最后 10 分钟」区分文案。
- 不应把 15:30 当作券商实际截止时间；不同券商可能提前关闭通道，UI 提示「以券商客户端为准」。

### 3.2 交易日历

- 逆回购随 A 股交易日开市，休市安排与股票一致。
- 交易所年度休市公告是权威数据源，例如上交所年度休市安排：[上交所休市安排](https://big5.sse.com.cn/site/cht/www.sse.com.cn/disclosure/dealinstruc/closed/)
- 周末永远休市；国务院调休形成的周六工作日，交易所仍不开市。因此不能复用「法定工作日」库。
- 年度安排通常在上一年年底发布，存在「新的一年尚未公布」的空窗。必须设计覆盖年份与过期提示，而不是猜测。
- **首次开放门槛**：逆回购功能第一次对用户开放时，内置日历必须覆盖未来至少 12 个月；若下一年的交易所公告尚未发布，代码可以合并但功能保持默认关闭。该门槛只约束“逆回购首次可用”，不约束信用卡修复或普通维护版本。不得用预测日期填充，不得在覆盖不足时静默放行。
- 功能已存在后，每个版本不强制重新满足 12 个月；但覆盖不足 60 天时必须持续告警，且覆盖结束当天起自动停止逆回购提醒。信用卡提醒不依赖交易日历，可独立发布。
- 日历随 EXE 版本发布，不引入旁挂文件或运行时下载。代价是每年需要一次应用更新，这与项目现有的单 EXE 发布和更新提示一致；若未来要免除升级，必须单独立项设计签名日历包，并同步修改打包 allowlist，不能在本方案里含糊带过。

### 3.3 信用卡规则

- 还款日由发卡行合同约定，是否遇周末/节假日顺延因行而异，不能统一推断。
- 29/30/31 日在短月通常按当月最后一天处理是常见产品做法，但仍应在 UI 中展示下一期实际日期，让用户自行核对。
- 逾期宽限期、容时服务因行而异，本程序不计算、不承诺，只显示用户配置的还款日。

### 3.4 GitHub 参考项目

| 项目 | 可借鉴 | 不可直接照搬 |
| --- | --- | --- |
| [IndigoMagic/NationalDebtRRR](https://github.com/IndigoMagic/NationalDebtRRR) | 逆回购阈值提醒、PushDeer 外发 | README 明确「没有做工作日判断」；仅按利率阈值，不是交易日尾盘调度 |
| [Soli22de/treasury-repo-alert](https://github.com/Soli22de/treasury-repo-alert) | 节前/季末窗口的产品思路；数据源失败时仍给出日历结论 | 触发日期手工硬编码到 2027；README 对周五计息描述有误；无许可证，不能复制代码 |
| [20797618-prog/credit-card-reminder](https://github.com/20797618-prog/credit-card-reminder) | 月末钳制、`lastPaidDue` 指针、`paidHistory`、东八区日期测试 | 只有倒计时看板，无本地通知调度；使用 `lastPaidDue` 会让长期未确认时难以推进下一期；无许可证 |
| [ASHWINI-GUPTA/Card-Nudge](https://github.com/ASHWINI-GUPTA/Card-Nudge) | 默认 T-3 + 到期日两次本地通知；离线优先 | 移动端 Flutter；依赖 Supabase；产品成熟度不足以作为规则来源 |
| [meceware/wapy.dev](https://github.com/meceware/wapy.dev) | 每个周期多档通知时间、已支付动作、通知时间可配置 | Web/Postgres/邮件推送架构；许可证为 MIT + Commons Clause，不能按普通 MIT 复制 |
| [shawn666liu/tradecalendar-rs](https://github.com/shawn666liu/tradecalendar-rs) | Rust + CSV 内置日历、年度手工更新流程 | 仅约 5 stars，数据需自行核验；引入第三方 crate 会增加供应链和年度升级耦合 |
| [jiangzhuochi/trading_day_calc](https://github.com/jiangzhuochi/trading_day_calc) | 双交易所结果必须一致、失败保留旧快照、越界才联网刷新 | Python 实现；第一版不引入运行时联网日历 |

调研结论：可以借鉴**数据模型和交互**，但没有一个项目同时满足「Windows 本地常驻、真实交易日历、多类型提醒、隐私边界」。本方案不直接复用上述代码。

---

## 4. 产品与交互设计

### 4.1 导航

保留现有页索引 0–3 不动，新增：

| active-page | 页面 | 导航顺序 |
| --- | --- | --- |
| 0 | 今日任务 | 1 |
| 1 | 未来 60 天 | 2 |
| 4 | 逆回购 | 3 |
| 5 | 信用卡 | 4 |
| 2 | 健康与诊断 | 5 |
| 3 | 设置 | 6 |

这样新增页面不会改变现有托盘、设置页、更新页对 `active-page` 的假设；导航显示顺序通过显式按钮映射实现。Rust 侧引入 `const PAGE_REPO: i32 = 4; const PAGE_CARDS: i32 = 5;`，禁止继续散落魔法数字。

紧凑宽度下 Tab 文案使用「逆回购」「信用卡」，不添加图标，避免 800px 下挤压。

### 4.2 逆回购 Tab

页面从上到下：

1. **状态卡**（必须覆盖完整组合，避免实现时各写一套文案）

| 条件 | 状态卡文案 |
| --- | --- |
| 总开关关闭 | `逆回购提醒已关闭` |
| 日历未覆盖今天 | `交易日历未覆盖今天 · 暂不提醒` |
| 今天休市 | `今天休市 · 下次提醒 <下一交易日> <下一时点>` |
| 今日已处理 | `今日已处理 · 不再补提醒`；提供「撤销今日已处理」 |
| 当前时点有效窗口内、尚未展示 | `今天交易日 · <时点> 提醒待处理` |
| 两个时点都在未来 | `今天交易日 · 下次提醒 14:50` |
| 14:50 已过、15:20 开启且未到 | `今日主提醒已过 · 15:20 还有一次兜底` |
| 只开 15:20 且当前未到 15:20 | `今天交易日 · 下次提醒 15:20` |
| 只开 14:50 且已过 15:00 | `今日提醒已结束（<主提醒时点>）` |
| 只开 15:20 且已过 15:30 | `今日提醒已结束（<兜底时点>）` |
| 日历覆盖内但 45 天无交易日 | `近期无交易日 · 请检查日历数据` |
| 15:20 补发时已不足 10 分钟 | 文案按实际剩余分钟数生成，不写「还有 10 分钟」 |
2. **提醒开关**
   - `启用逆回购提醒` 总开关。
   - `14:50 主提醒（A 股尾盘）`，默认开。
   - `15:20 兜底提醒（逆回购最后 10 分钟）`，默认关，可单独开。
   - 自定义时间范围 13:00–15:25；主提醒与兜底时间不得相同；`valid_until = min(时点 + 10 分钟, 15:30)`。
   - **v1 只覆盖下午时段**：产品目标是收盘前处理闲置资金，不是全天现金管理；上午 09:30–11:30 属于明确不做范围，避免把功能扩张成盘中资金管家。13:00 是下午连续匹配开始时间，15:25 是留出 5 分钟操作余量的上限。
   - 关掉最后一个时点时，自动关闭总开关，并提示「已关闭逆回购提醒」，不留「总开关开着但永不触发」的隐式状态。
   - 用户自定义时间后，标签退化为「主提醒」「兜底提醒」，正文不再写死「A 股 15:00 收盘」，避免文案与自定义时间矛盾。
   - 配置只对保存后的**未来时点**生效；任何 off→on 转换（总开关或单个时点）都刷新 `rules_changed_at=now`，已经过去的时点不补响，未来时点照常。
   - 仅切换总开关或修改无关字段也不得吞掉当天尚未展示的提醒：off→on 只跳过“已经过去的时点”，不会影响 14:50 之前的启用，也不会影响次日。
   - 时间只允许 13:00–15:25，预设之外可通过 `HH:mm` 自定义；非法输入即时提示。信用卡提醒时间上限另见 4.3。
3. **说明卡**
   - 官方名称：债券通用质押式回购逆回购（俗称国债逆回购）。
   - 沪深代码：GC001 / 204001，R-001 / 131810；1000 元起。
   - 「逆回购操作方向为卖出（出借资金），请以券商客户端为准。」
   - 「14:50 是偏利率的提前提醒，不保证利率更高；逆回购可交易至 15:30。」
4. **日历状态**
   - `交易日历：已覆盖至 <日期>（发布时至少未来 12 个月），数据来自沪深交易所年度休市公告`。
   - 覆盖不足时黄色提示，不弹窗；剩余 60 天时同时在主界面健康卡和托盘显示警告。
   - 发布前必须覆盖未来至少 12 个月，否则功能不得上线；不得用预测节假日填充空白年份。
5. **今日操作**
   - `今日已处理`：仅当今天是交易日且存在今日提醒时可用；点击后取消今日后续逆回购提醒，并写本地确认记录。
   - 不提供「已成交」「已获利」等无法验证的状态。

通知文案示例：

```text
标题：逆回购提醒 · 14:50
正文：今天是交易日。若账户有闲置资金，可在券商客户端查看 GC001 / R-001；
      逆回购可交易至 15:30。14:50 是偏利率的提前提醒，不保证利率更高；
      利率随资金面波动，请自行判断。
按钮：今日已处理 / 稍后 / 打开逆回购页
```

15:20 文案改为「逆回购最后 10 分钟」，并按实际剩余时间显示「距离 15:30 还有 N 分钟」；不重复完整科普。正文不能出现「稳赚」「无风险」「利率更高」等绝对化表达。自定义时间时，标题使用实际时点，正文不再断言 A 股收盘时间。

### 4.3 信用卡 Tab

页面从上到下：

1. **摘要卡**：`2 张待处理 · 最近 11 月 3 日（3 天后）`；逾期存在时优先显示红色「1 张已逾期 2 天」。
2. **卡片列表**：按「逾期 > 今天到期 > 最近到期 > 已暂停」排序。
   - 行内显示别名、银行（可选）、下一还款日、剩余/逾期天数、已启用提醒档位。
   - 操作：`本期已还`、`编辑`、`暂停`、`删除`。
3. **新增/编辑面板**（内联抽屉或覆盖层，不新开窗口）：
   - 别名（必填，1–30 字）。
   - 银行（可选，1–20 字）。
   - 每月还款日（1–31），下方实时预览「下一期：2027-02-28（当月最后一天）」。
   - 提醒时间（默认 09:00，允许 08:00–22:00，避免 23:59 与 60 秒心跳/跨日边界冲突）。
   - 提前提醒：7 / 3 / 1 / 当天，多选，默认 3 天 + 当天；最多 4 个。
   - `到期未确认时，次日再提醒一次`，默认开。
   - 隐私提示：「只保存在本机，请勿填写完整卡号或安全码。」
4. **空态/零值态**：
   - 没有卡片：`还没有信用卡提醒。添加一张卡，程序只记录别名和日期。`
   - 有卡片但全部暂停：`所有信用卡提醒已暂停`。
   - 有卡片但都还没到首个跟踪账期：`暂无临近还款日`，不显示「逾期 0 天」。
   - 摘要卡 0 张待处理时显示「暂无待处理」；全部暂停单独标识，不能和「已还清」混淆。
5. **历史记录**：可折叠，显示「11 月 3 日 · 已还 · 11 月 2 日操作」；支持撤销最近一次误操作。

状态语义：

| 状态 | 条件 | 颜色/动作 |
| --- | --- | --- |
| 已暂停 | `enabled=0` | 灰色，无提醒 |
| 未开始 | 今天所在账期早于 `first_tracked_cycle` | 中性，不虚构逾期 |
| 正常 | 距下一还款日 > 7 天 | 中性 |
| 临近 | 1–7 天 | 黄色 |
| 今天到期 | 0 天 | 橙色，置顶 |
| 已逾期 | 最早未结清期已过 | 红色，显示逾期天数 |
| 本期已处理 | `settled_through` 已覆盖到当前期 | 绿色，滚到下一期 |

**新卡基线**：建卡时记录 `first_tracked_cycle`。若今天不晚于本月钳制后的还款日，则本月，否则下月；今天就是还款日时仍算本月。用户 5 号添加一张 5 号还款的卡，列表显示「今天到期」；6 号起才可能逾期。今天 10:00 才添加、09:00 提醒已过时，当天不补通知，但状态仍真实显示到期日。所有逾期判断从 `max(first_tracked_cycle, settled_through 后一个账期)` 起算，且独立于提醒枚举。

**暂停与恢复**：`enabled` 从 0→1 时刷新 `rules_changed_at=now`，恢复后不补暂停期间已经过去的时点；恢复时若今天 09:00 已过，不立刻弹「今天到期」，但列表仍显示真实到期状态，次日的逾期提醒按新规则计算。未来时点照常。

**「本期已还」的语义**：按钮必须携带 `cycle_key`（如 `2026-11`），确认文案写「将 2026 年 11 月及之前各期标记为本机已处理」。若连续两个月未确认，点击一次会把 `settled_through` 推进到该账期，历史只记一条动作，避免要求用户补点两次；撤销时删除该动作，`settled_through` 自动回退到上一条 `MAX(cycle_key)`。该操作只更新本地提醒状态，不代表银行入账。

通知文案示例：

```text
标题：信用卡还款提醒 · 3 天后
正文：招行 Visa 的还款日是 11 月 3 日（周二），还有 3 天。
      本提醒按自然日计算；请以发卡行账单为准。
按钮：本期已还 / 稍后 / 打开信用卡页
```

到期日当天标题用「今天到期」；逾期次日用「已逾期 1 天」。正文不含金额和卡号。

**Toast 与专用窗口分开脱敏**：专用窗口可以显示别名；Windows Toast 正文只写「信用卡还款提醒 · 3 天后」或「信用卡还款提醒 · 今天到期」，不写银行和别名，因为系统通知历史可能经 Phone Link/云同步离开本机。用户点击 Toast 进入应用后再看到具体卡片。

### 4.4 呈现层级

- 逆回购、信用卡复用现有专用提醒窗口，但窗口必须支持 `kind/source-id/primary-action`，不能把 `finance:*` 当股票 ID；具体改造见 5.7。
- 默认弹出专用窗口，因为这两类提醒对时点敏感；声音和 Toast 仍受全局 `sound_enabled` / `toast_enabled` 控制。
- Toast 点击路由：
  - `finance:repo` → 打开逆回购 Tab。
  - `finance:card:<uuid>` → 打开信用卡 Tab，并定位到该卡。
  - 普通无前缀参数继续按 IPO 事件处理，保持向后兼容。
- **14:50 落在 IPO 尾盘提醒区间，撞车概率高**：`safety_cutoff` 默认 14:55，IPO 在 14:45–14:55 有 2 分钟级提醒、到 14:55 前每 5/15 分钟还有多档，14:50 前后几乎必然有窗口。不能直接覆盖正在显示的 IPO 窗口，否则会改掉「确认已申购」的对象。
- **v1 采用串行提醒队列，不做跨类型正文合并、不冻结混合按钮**：
  - 同一时刻只显示一个专用提醒窗口；finance 永不覆盖 IPO。
  - 若当前是 IPO 窗口而 finance 到达：先按全局设置发一条**通用 Toast + 声音**（「逆回购提醒 · 14:50」或「信用卡还款提醒 · 今天到期」），详细窗口排队到当前窗口关闭后再显示。这样即使队列延迟，用户也先收到时点信号。队列 item 标记 `alert_sent=true`，之后显示详细窗口时不再重复声音/Toast。
  - 若当前是 finance 窗口而 IPO 到达：**IPO 永远抢占，IPO 永不排队**。这样 IPO 继续沿用原 Outbox 的 2 分钟租约、失败重试和过期语义，不会套用 finance 的十分钟规则。
    - finance 尚未完成可见确认：收起窗口，item 留在队列/in-flight，不写 shown；IPO 关闭后重新显示。
    - finance 已确认可见：收起窗口，视为已送达；**不入队、不重显**，因为 `finance_shown` 已写入，重显会违反去重语义。
    - 无论哪种情况，IPO 窗口都不被 finance 修改 `event_id/event_version/can_acknowledge`。
  - IPO 的交付完成仍只由它自己的 `presentation_id` 可见确认触发；任何 finance 抢占路径不得提前 `complete_delivery`，也不得修改 IPO Outbox 状态。
  - 队列明确按到达顺序，不混排类型；每条 item 只存路由与时效字段（`kind/source_id/occurrence_key/due_at/valid_until`）以及 `alert_sent`，不存正文；正文在真正显示时按当前配置重新渲染，避免别名/日期改动后显示旧文案。
  - 队列 item 在显示前重新校验：来源已暂停/删除、规则已变更、key 已展示或已过窗，则丢弃并调用 `finance_discard(keys)` 释放 in-flight，不再重试。
  - 每条 item 记录 `valid_until`，过窗则丢弃并释放 in-flight。
  - 窗口关闭、按「知道了/稍后」、完成 IPO 主操作、隐藏窗口后，都必须调用 `drain_queue()`；`drain_runtime_ui` 末尾再兜底一次。
  - 这样把「窗口属性泛化」和「IPO 确认路径」分开：IPO 批次仍保持原来的单类型语义，finance 批次不会出现「混合窗口该确认谁」的歧义。
  - **健康摘要是第三个窗口生产者，必须一起收口**：任何专用提醒窗口可见时，健康摘要不再 `set_reminder_title/body` 覆盖窗口，只更新健康页/托盘文字；需要提示时发通用 Toast。禁止它把 IPO 的 `event_id/can_acknowledge` 清空，或覆盖尚未完成可见确认的 finance 窗口。
  - **设置页“提醒窗口测试”是第四个窗口生产者**：测试前必须检查窗口占用；占用时提示「先关闭当前提醒」，不得覆盖正式提醒。测试展示前必须 `next_presentation_id()`，避免旧的 150ms 回调把测试内容误认成正式 finance 已展示。
  - 仍需新增独立的 `primary_action(kind, source_id, cycle_key)` 回调，不能复用 `request-acknowledgement(event-id, version)`。
- 第二通知通道默认不发送这两类提醒。若未来开放，必须单独 opt-in 并明确「卡片别名会离开本机」。

---

## 5. 技术方案

### 5.1 总体数据流

```text
配置表 + 交易日历 + 已处理台账
        │
        ▼
finance::schedule::due_candidates(now)      // 当前有效窗口内、尚未展示
finance::schedule::next_future_slot(now)    // 严格 > now 的下一次计划
        │
        ├─ due_candidates 非空 → 立即进入本次交付周期
        └─ next_future_slot → 加入 Runtime WakeDeadline
        │
        ▼
ReminderCoordinator 串行队列（当前窗口不被覆盖）
        │
        ├─ 当前有窗口 → finance Toast/声音先提示，详细 item 入队
        ├─ 无窗口 → 先加入内存 in_flight_keys，60 秒心跳不会重复投递
        ├─ 窗口可见 → INSERT finance_shown，并转入 shown_this_session
        ├─ 用户稍后 → 同样视为已展示，不重复
        └─ 窗口失败 → 从 in_flight 移到 retry_pending[key]=now+30s，最多重试到 valid_until
```

IPO 的 `reminder_outbox` 保持不动；新功能不建一个「完整 Outbox」，而是用**纯函数计划器 + 已展示台账**。理由：

- 逆回购的有效窗口只有 10 分钟，信用卡只要求当天送达；跨天持久化重试没有意义。
- 应用是单实例，窗口在本地呈现；最需要保证的是「不重复」和「不在窗口外补发」，不是远程投递语义。
- 这样避免 Pending/Leased/Failed/Cancelled 状态机、租约、复活、退避等一整套复杂度，且天然避免「Cancelled 行占住唯一键导致新配置无法插入」。
- 唯一需要持久化的是「这条已经展示过」和「今天已处理/这一期已还」，业务状态与展示状态分离。

### 5.2 交易日历模块

新增 `src/finance/calendar.rs`（不另设 `src/trading_calendar.rs`，避免两个入口）：

```rust
pub struct TradingCalendar {
    coverage_start: NaiveDate,
    coverage_end: NaiveDate,
    closed_weekdays: BTreeSet<NaiveDate>,
    source_published_on: NaiveDate,
    source_label: String,
}

impl TradingCalendar {
    pub fn is_trading_day(&self, date: NaiveDate) -> Option<bool>;
    pub fn next_trading_day(&self, date: NaiveDate) -> Option<NaiveDate>;
    pub fn coverage(&self) -> (NaiveDate, NaiveDate);
}
```

规则：

```text
is_trading_day =
    日历覆盖该日期
    && 周一至周五
    && 不在交易所休市日集合
```

- 周六周日即使国务院调休上班，也返回 `false`。
- 日期不在覆盖范围返回 `None`，调用方 fail-closed。
- 数据以编译期 JSON 嵌入：`assets/trading-calendar/cn-exchange.json` + `include_str!`。单 EXE 发布布局不变。
- JSON 同时记录沪深公告 URL、公告日期、抓取/录入日期；加载时校验日期合法、年份连续、无重复、覆盖范围。
- 每年交易所公告发布后更新 JSON 并发版；日历只能随 EXE 更新，不设旁挂文件。现有更新胶囊负责提示新版本，但不承诺自动安装。
- 覆盖结束前 60 天开始，在逆回购 Tab、主界面健康卡和托盘中同时显示警告；覆盖结束后不提醒。不能只藏在用户不常打开的 Tab 里。
- **首次开放硬门槛**：逆回购首次对用户开放时，内置日历覆盖未来至少 12 个月；若官方下一年安排尚未发布，代码可合并但逆回购功能保持默认关闭。信用卡不依赖此门槛。
- 功能开放后，普通维护版本不重复要求 12 个月，但覆盖剩余 60 天起必须持续告警，覆盖结束后自动停止逆回购提醒。
- 第一版不做运行时联网抓取。理由：交易所页面结构变化、网络异常、代理和证书问题都会把「可靠提醒」变成新的故障源。后续如要联网，必须按 `trading_day_calc` 的原则：沪深两份结果一致、全量校验通过、原子替换、失败保留旧快照；同时单独评估签名和打包。
- 按记录日 2026-10-31，当前只内置 2026 年官方安排。实现可以完成，但逆回购默认关闭；拿到 2027 年公告并补齐数据前，不把“已开放逆回购”作为发布声明。

日历测试必须包含：

- 普通周一至周五。
- 普通周末。
- 法定节假日中的工作日。
- 调休周六（仍休市）。
- 元旦、春节、清明、劳动节、端午、中秋、国庆边界。
- 覆盖范围前后一天返回 `None`。
- 闰年 2 月 29 日。

### 5.3 已展示台账

v12 新增，不触碰 IPO 表：

```sql
CREATE TABLE finance_shown(
    kind TEXT NOT NULL CHECK(kind IN ('reverse_repo','credit_card')),
    source_id TEXT NOT NULL,
    occurrence_key TEXT NOT NULL,
    event_date TEXT NOT NULL,          -- 北京日期 YYYY-MM-DD
    shown_at TEXT NOT NULL,
    PRIMARY KEY(kind, source_id, occurrence_key)
);
CREATE INDEX ix_finance_shown_date
    ON finance_shown(event_date);
```

设计要点：

- `occurrence_key` 是稳定身份：逆回购 `2026-11-02:14:50`；信用卡 `2026-11:3:2026-11-28`、`2026-11:overdue:2026-11-28`。信用卡 key 包含账期、档位和**该账期实际还款日**：修正还款日后新日期的剩余档位会作为新 key 重新提醒，旧 key 不会复活；正常未改日期时不会产生重复。
- 只在**专用提醒窗口确认可见后** `INSERT OR IGNORE`。重启、时钟回拨、重复唤醒都不会再次展示同一 key。
- 不存标题/正文/状态；配置改动后旧 key 自然失效，不会复活旧文案。
- 计划器不依赖这张表做路由，只把它作为「该 key 已经展示过」的过滤器；若某行对应的配置已删除，清理即可。
- 每日维护删除 `event_date < today - 90 天` 的记录；它只用于短期去重，不需要永久保留。
- 不接受「窗口失败也写入」：失败时本次不写，由按 key 的 `retry_pending` 重试；重启后若仍在有效窗口，纯函数计划器会重新选中它。
- Runtime 内存维护三组状态：`in_flight_keys`（已交给呈现/队列）、`shown_this_session`（本次已成功展示）、`retry_pending: BTreeMap<Key, ChinaDateTime>`（按 key 的失败重试时刻）。
- 计划器过滤条件：`key ∈ finance_shown ∪ shown_this_session` 时永不投递；`key ∈ in_flight_keys` 时本轮不重复投递；`key ∈ retry_pending` 且 `retry_at > now` 时本轮不投递；其余候选可投递。
- 进入协调器时立即加入 `in_flight_keys`，因此 60 秒心跳不会重复投递同一 item，也不会把队列每分钟塞一条。
- 呈现成功：`RuntimeHandle::finance_presented(presentation_id, keys)` 先核对 presentation 仍是当前窗口，再写 `finance_shown`，从 `in_flight_keys` 和该 key 的 `retry_pending` 清除，并转 `shown_this_session`。
- 呈现失败：`finance_present_failed(presentation_id, keys, category)` 同样核对后把 key 从 `in_flight_keys` 移到 `retry_pending[key]=min(now+30s, valid_until)`，并唤醒 Runtime。失败 key 仍被 `retry_pending` 过滤，不会紧循环。
- 队列项若超过 `valid_until`，显式转为「本次会话放弃」，从 in_flight/retry 移除并清理；不静默滞留。
- `next_finance_wake` 取「下一次未来计划时点」与「retry_pending 中最早的未来时刻」的较小值；所有 `retry_at <= now` 的条目要么在本轮变为可投递，要么因过期被清除，绝不允许返回过去时间。
- 若 SQLite 写入短暂失败，保留 `shown_this_session`，本次运行不重复；下次启动再补写/补投，最多造成一次跨重启重复，换取不丢提醒。

这张表替代了完整的 occurrence 状态机。`finance_shown` 没有 Pending/Leased 等状态，因此不存在旧方案中 `Cancelled` 行占住唯一键、`Delivered` 被 upsert 重置、窗口外 Pending 永不过期这三类缺陷。

### 5.4 配置表

CREATE TABLE reverse_repo_settings(
    id INTEGER PRIMARY KEY CHECK(id=1),
    enabled INTEGER NOT NULL DEFAULT 0,
    primary_time TEXT NULL DEFAULT '14:50',  -- NULL = 主提醒关闭
    fallback_time TEXT NULL DEFAULT NULL,    -- NULL = 兜底关闭
    rules_changed_at TEXT NOT NULL,          -- 时间变更或 enabled off→on 时更新
    updated_at TEXT NOT NULL
);

CREATE TABLE reverse_repo_day_acks(
    trade_date TEXT PRIMARY KEY,       -- YYYY-MM-DD
    acted_at TEXT NOT NULL
);

CREATE TABLE credit_cards(
    id TEXT PRIMARY KEY,
    alias TEXT NOT NULL,
    bank TEXT NULL,
    due_day INTEGER NOT NULL CHECK(due_day BETWEEN 1 AND 31),
    remind_time TEXT NOT NULL DEFAULT '09:00',
    lead_days_json TEXT NOT NULL DEFAULT '[3,0]',
    overdue_enabled INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1,
    first_tracked_cycle TEXT NOT NULL, -- YYYY-MM；创建时的首个跟踪账期
    rules_changed_at TEXT NOT NULL,    -- due_day/remind_time/lead_days/overdue 或 enabled off→on
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL           -- 任一字段编辑，含别名
);

CREATE TABLE credit_card_actions(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    card_id TEXT NOT NULL REFERENCES credit_cards(id) ON DELETE CASCADE,
    cycle_key TEXT NOT NULL,           -- YYYY-MM，账期身份，不随还款日改动
    due_on TEXT NOT NULL,              -- 该账期最终展示的还款日
    action TEXT NOT NULL CHECK(action IN ('paid')),
    acted_at TEXT NOT NULL
);
CREATE UNIQUE INDEX ux_card_action_cycle
    ON credit_card_actions(card_id, cycle_key);
```

- `reverse_repo_settings` 单行初始化，迁移默认 `enabled=0`。现有用户升级后不会突然收到新提醒，进入 Tab 一键开启。
- `primary_time/fallback_time` 直接映射 Rust 的 `Option<NaiveTime>`，不再维护「enabled + time」两套字段；「至少一个非 NULL」由应用层校验和 UI 保证。
- **规则变更时间与普通编辑分离**：`rules_changed_at` 在时间/还款日/提前档位/逾期开关变化，以及 `enabled off→on` 时刷新；只改别名、银行、总开关关闭不刷新。计划器用它判断「这个时点是否在规则生效前已过去」：改卡名不会吞掉当天尚未展示的提醒，重新启用则不会补响已过时点。
- `lead_days_json` 服务端解析并校验：去重、升序、0–30、最多 4 个，UI 只提供 7/3/1/0。
- `credit_cards.alias` 去空白、限制长度；SQL 参数绑定，UI/Toast 分别做 Slint 文本和 XML 转义。
- **账期身份用 `cycle_key=YYYY-MM`，不是裸日期**：用户在同一月把还款日从 5 号改成 28 号，已结清的 11 月不会因新日期再次生成提醒；`due_on` 只用于展示和记录。
- `settled_through = MAX(credit_card_actions.cycle_key)`，按账期比较，不按钳制后的日期比较。撤销即删除最新一行，自动回退。
- `first_tracked_cycle` 在创建时确定：若今天不晚于本月钳制后的还款日，则本月，否则下月；今天就是还款日时仍算本月。
- 不把配置放进 `AppSettings`，因此「恢复默认设置」不会清空卡片，也不会触发 IPO 重排/同步。
- 删除卡片级联删除 actions 和 shown（在事务中显式删除，SQLite 外键需保持启用）。

### 5.5 调度算法

#### 逆回购

新增 `src/finance/repo.rs`：

```rust
pub struct RepoPreferences {
    enabled: bool,
    primary: Option<NaiveTime>,   // 14:50
    fallback: Option<NaiveTime>,  // 15:20
    rules_changed_at: ChinaDateTime,
}

pub fn valid_until_for(date: NaiveDate, slot: NaiveTime) -> ChinaDateTime {
    min(at(date, slot) + 10.minutes(), at(date, time(15, 30)))
}

pub fn due_candidates(
    now: ChinaDateTime,
    prefs: &RepoPreferences,
    calendar: &TradingCalendar,
    acked: &BTreeSet<NaiveDate>,
    shown: &FinanceShown,
) -> Vec<RepoOccurrence>;

pub fn next_future_slot(
    now: ChinaDateTime,
    prefs: &RepoPreferences,
    calendar: &TradingCalendar,
    acked: &BTreeSet<NaiveDate>,
) -> Option<ChinaDateTime>;
```

伪代码：

```text
if !enabled: return empty / None
for day in 0..=LOOKAHEAD_DAYS:
    date = today + day
    if calendar.is_trading_day(date) != Some(true): continue
    if date in acked: continue
    for time in [primary, fallback].filter(non-null).sorted:
        slot = at(date, time)
        if slot < rules_changed_at: continue  // 规则变更不追溯当前窗口
        valid_until = min(slot + 10min, at(date, 15:30))
        if slot <= now < valid_until and not shown(kind, source, key):
            candidates.push(slot)
        else if slot > now:
            return earliest future slot
return None
```

关键行为：

- 每个时点各有自己的 `valid_until`：14:50 到 15:00，15:20 到 15:30；自定义时点按 `min(slot+10m,15:30)`。
- 14:50 已过 15:00 后不再补发；若 15:20 开启，由 15:20 接棒。
- 若 14:50 已展示但用户未点「今日已处理」，15:20 仍按配置触发。
- 若用户点「今日已处理」，向 `reverse_repo_day_acks` 写当天日期，计划器跳过当天全部时点；撤销只删除该行，已经展示过的时点仍由 `finance_shown` 去重，只有尚未展示的后续时点可能恢复。
- 配置保存后，计划器只接受 `slot >= rules_changed_at` 的时点；避免把时间从 14:50 改到 14:45 时立刻补响，也避免 15:21 才打开 15:20 时补响。关闭总开关本身不刷新；`enabled off→on` 或单时点 off→on 刷新为 now，已过时点不补、未来时点照常。
- 系统时间回拨：`finance_shown` 主键阻止重复；系统时间前拨越过 `valid_until`：不补发。
- 跨午夜、休眠恢复、跨年由统一 `now_china()` 和日历覆盖判断处理。
- `LOOKAHEAD_DAYS = 45`，足以跨春节等最长休市；若 45 天内无交易日，状态卡明确显示「近期无交易日」，不是「无提醒」。

#### 信用卡

新增 `src/finance/credit_card.rs`：

```rust
pub fn due_on_for_month(year: i32, month: u32, due_day: u32) -> NaiveDate; // clamp
pub fn cycle_for_date(card: &CreditCard, date: NaiveDate) -> String; // "YYYY-MM"
pub fn earliest_unsettled_cycle(
    card: &CreditCard,
    settled_through: Option<&str>, // "YYYY-MM"
    today: NaiveDate,
) -> Option<String>;
pub fn due_candidates(
    now: ChinaDateTime,
    card: &CreditCard,
    settled_through: Option<&str>,
    shown: &FinanceShown,
) -> Vec<CreditCardOccurrence>;
```

伪代码：

```text
if !card.enabled: return empty
for cycle in previous_month + current_month + next_3_months:
    if cycle < first_tracked_cycle: continue
    if settled_through exists and cycle <= settled_through: continue
    due = min(due_day, last_day_of_month(cycle))
    for lead in lead_days:
        slot = at(due - lead days, remind_time)
        if slot < card.rules_changed_at: continue   // 规则变更不追溯
        valid_until = end_of_day(slot.date)         // 当天 23:59:59
        key = cycle + ":" + lead + ":" + due
        if slot <= now < valid_until and not shown(key):
            candidates.push(slot,key)
        else if slot > now: next_future = min(next_future, slot)
    if overdue_enabled:
        slot = at(due + 1 day, remind_time)
        if slot < card.rules_changed_at: continue   // 恢复/启用不补已过时点
        key = cycle + ":overdue:" + due
        if slot <= now < end_of_day(slot.date) and not shown(key):
            candidates.push(slot,key)
        else if slot > now: next_future = min(next_future, slot)
return unique(candidates), next_future
```

关键行为：

- 每张卡最多 4 个提前档位 + 1 个逾期提醒；key 用 `(cycle_key, lead, due_on)` / `(cycle_key, overdue, due_on)`。正常排程同一账期各档位日期不同，不会因同日误合并；**修正还款日属于显式重排**：新 `due_on` 生成新 key，同账期剩余档位重新提醒，旧 key 不会复活。该行为是产品决策，不是副作用，必须有测试。
- 编辑卡片只影响未来：修改 `due_day/remind_time/lead_days/overdue_enabled` 刷新 `rules_changed_at`；只改别名/银行不刷新，也不会吞掉当天尚未展示的提醒。
- 暂停卡片只是 `enabled=0`，计划器返回空；恢复后从当前时间重算，不补暂停期间提醒。
- 删除卡片事务内删除 actions 和 shown；没有 occurrence 需要取消。
- `settled_through = MAX(credit_card_actions.cycle_key)`；只阻断 `cycle_key <= settled_through`，未来账期仍独立计算，避免「忘了点已还就永远不再提醒下一期」。
- 逾期提醒只在 `due+1` 当天一次。界面逾期状态由 `earliest_unsettled_cycle()` 实时计算，不依赖提醒是否发送。
- 新建卡片在创建前已经过去的时点不补发；否则刚添加一张 5 号卡就会立刻弹「今天到期」或更早的提前提醒。未来账期照常。
- 提醒时间早于当前时点但仍在当天有效窗口内，且规则早于该时点，补发一次；已过当天 23:59:59 不再补发。提醒时间上限 22:00。
- 信用卡提醒不查交易日历，周末和节假日照常提醒。
- 计划器至少扫描**上个月、当月和未来 3 个月**。只从当月开始会漏掉「2 月 28 日到期、3 月 1 日逾期」和「12 月 31 日到期、1 月 1 日逾期」这两类跨月提醒。状态判断不受 3 个月窗口限制。
- 逾期分支同样必须执行 `slot >= rules_changed_at`；否则在逾期日当天恢复卡片或开启逾期提醒时，会立刻补发当天已过的提醒，与「恢复后不补已过时点」矛盾。

### 5.6 Runtime 集成

**硬约束：deadline 函数永远不能返回 `<= now`。** 现有 `duration_until` 对过去时间返回 0，`run_loop` 会立即重入；如果补发判定也返回 `now`，会满速空转且不投递。到期判断与「下一次唤醒」必须分离。

**网络隔离硬约束**：当前 `run_loop` 同步抓取多个来源，单请求超时可达 45 秒；金融提醒不能与同步共用阻塞路径。

- 金融到期判断必须在任何同步调用之前执行。
- `synchronize` 移入独立 `stock-ipo-sync` 线程，Runtime 主循环只发送任务并接收 `SyncFinished`，绝不 `join` 等待。
- 同步进行中时，deadline 不得继续指向已到期的同步时刻；使用 `now + 60s` 占位，避免忙等。
- 同步完成通过 `RuntimeCommand::SyncFinished` 唤醒主循环，再处理成功/失败/重试和 UI 刷新。
- 必须增加测试：模拟 45 秒阻塞同步时，金融提醒仍能在 14:50 窗口触发；至少证明 `run_loop` 不在同步线程上等待。

修改点：

1. `src/runtime/scheduler.rs`
   - 新增 `next_finance_wake(database, now, calendar, configs, retry_pending) -> Option<ChinaDateTime>`，取「严格大于 now 的下一次计划时点」与「retry_pending 中最早的未来时刻」的较小值，永不返回 `now` 或过去。`retry_pending` 必须显式入参，不能只写在说明里。
   - `WakeDeadline::consider` 加入 `"逆回购或信用卡提醒到期"`。
2. `src/runtime/run_loop.rs`
   - 每次循环先计算 `finance_due = eligible_due_candidates(now, in_flight, shown, retry_pending)` 是否非空；只有 `retry_at <= now` 的 key 重新变为 eligible。
   - 当前进入 `run_delivery_cycle` 的条件必须加上 `finance_due`，不能只依赖 IPO `next_local_delivery_at` / 第二通道；这是现有 `run_loop.rs:49-58` 的耦合点。
   - 配置保存、卡片 CRUD、标记已还、撤销后发送 `RuntimeCommand::Wake`，立即重算，不等心跳。
3. `src/runtime/delivery.rs`
   - 新增 `run_finance_delivery`：计划器返回 candidates → 标记 in_flight → 通过 `UiEvent::FinanceDue` 投给 UI；**不得在 Runtime 线程直接操作 Slint**。
   - 成功后写 `finance_shown` 并清该 key 的 retry；失败把 key 迁到 `retry_pending`，下次尝试 `min(now+30s, valid_until)`。
   - IPO 和第二通道的现有路径独立，失败互不阻塞。
4. `src/ui/runtime_bridge.rs`
   - 收集 IPO 与 finance 提醒，交给 `ReminderCoordinator`；成功后按每条 key 写 shown。
   - 失败不写 shown，交给 retry。
   - 不把 finance 提醒交给 `reminder_batch` 的 IPO 分组逻辑；统一走 `ReminderCoordinator` 的串行队列，不做跨类型合并。
   - 窗口关闭/隐藏/完成主操作后显式 `drain_queue()`；每次 `drain_runtime_ui` 末尾兜底排空。队列项超过 `valid_until` 时丢弃并释放 in_flight。
   - `UiEvent::Health` 也必须检查窗口占用：任何专用提醒窗口可见时只更新健康页/托盘，不覆盖窗口；必要时发通用 Toast。参考现有 `src/ui/runtime_bridge.rs:313-339` 的无条件 `set_*` 路径。
5. `src/runtime/handle.rs`
   - 暴露 `finance_settings`、`credit_cards`、`mark_card_paid`、`undo_card_action`、`ack_repo_today`、`unack_repo_today`、`save_repo_settings`。
   - 增加呈现回传：`finance_presented(presentation_id, keys)` 先核对当前 presentation 仍匹配，再写 `finance_shown` 并清理这些 key 的 in-flight/retry；`finance_present_failed(presentation_id, keys, error_category)` 同样核对后把这些 key 迁到 `retry_pending` 并 `Wake`；`finance_discard(keys)` 在配置失效/过窗时清除 in-flight，不写 shown、不重试。
   - **沿用现有同步 SQLite 调用**，不假装已有后台写线程；单次事务必须短小，禁止在 UI 线程做全表扫描/迁移。
   - 每次写后发送 `UiEvent::FinanceChanged`（或增加 `finance_revision`），驱动 `refresh_finance_ui`。
6. 启动恢复
   - 第一轮循环直接调用 `due_candidates(now)`；有效窗口内未展示的候选补发一次，窗口外自然不在候选中。
   - 不需要持久化 Expired 状态，也不需要扫描历史 Pending。
7. 时钟变化
   - `finance_shown` 防重复；若系统时间前拨越过 `valid_until`，候选自然为空。
   - 系统时间回拨到旧窗口时，已写 shown 的 key 不再显示；未写 shown 的会补一次，这是可接受的最多一次。
8. 可测试性
   - `next_finance_wake` 加单元测试：任意输入 `now` 返回值必须 `> now` 或 `None`。
   - Runtime 测试构造「deadline 在过去」场景，断言循环最多执行一次且不会忙等。

### 5.7 通知与窗口泛化

不复制 `ReminderWindow`。将现有窗口的最小必要属性泛化：

```text
reminder-kind: ipo | reverse_repo | credit_card
reminder-source-id
reminder-cycle-key
reminder-title
reminder-body
reminder-primary-action-label
reminder-primary-action-enabled
reminder-open-label
reminder-can-acknowledge
```

必须修改当前硬编码：

- `ui/main.slint:1779-1856` 的标题固定为「A 股打新提醒」、按钮固定「打开详情 / 确认已申购」、回调 `open-details(event-id)` 与 `request-acknowledgement(event-id, version)`。
- 新增独立回调 `primary-action(string kind, string source-id, string cycle-key)`；IPO、逆回购、信用卡各自路由，不共用 IPO 确认。
- `src/ui/callbacks.rs:11-24` 的 `on_open_details` 需要前缀路由：`finance:repo → PAGE_REPO`，`finance:card:* → PAGE_CARDS + 定位卡片`，其余保持 IPO。
- `src/native_tray.rs:192-207` 的 Toast 激活同样需要前缀判定；`finance:*` 不能进入 `show_event_details`，否则会出现「任务已不存在」。
- 窗口标题由 `batch_title` 参数传入；每次展示只属于一种 kind，不构造「今日提醒」混合窗口。
- 当前窗口是 IPO 时，新 finance 只入队；当前窗口是 finance 时，IPO 可直接抢占。finance 永不覆盖 IPO，IPO 永不排队。关闭/抢占后由 `drain_queue` 显示下一条。
- `event_id/event_version` 只在 IPO 展示时设置；finance 展示时为空，不能落进 `show_event_details`。
- IPO 的 `complete_delivery` 也必须带自己的 `presentation_id` 并在可见确认时核对；抢占/排队路径不得提前完成 IPO delivery，IPO Outbox 的租约、重试、过期完全沿用原逻辑。
- 每次 `show_dedicated_reminder` 分配单调递增的 `presentation_id`；150ms 可见确认回调必须携带该 id，并在写入前核对当前 id。被 IPO 抢占、被新窗口替换或已关闭时，旧回调只能 no-op，不能把旧内容的 key 记成已展示。`finance_presented` 只在核对该 id 通过后调用。
- **150ms 内关闭窗口**：可见性确认失败 → `finance_present_failed`，key 进入 `retry_pending`（30 秒后、不超过 valid_until）；不得留在 in_flight 永不释放。关闭/隐藏回调必须 `notify_ui()`，让队列中的下一条继续排空。
- 队列 item 在每次显示前复核当前配置和 valid_until；已暂停/删除/规则变更/过窗则 `finance_discard`，不再显示。
- 这是对现有 IPO「可见后 complete_delivery」路径的顺带修正：当前实现只检查窗口是否可见，不检查窗口内容是否还是同一批 delivery。
- 新增 `reminder-open-label`，按 kind 显示「打开详情」「打开逆回购」「打开信用卡」；不能继续用 `event_id == "" ? 打开今日任务 : 打开详情` 的二分文案。
- IPO 仍使用「确认已申购」和事件详情。
- 逆回购显示「今日已处理」「打开逆回购」。
- 信用卡显示「本期已还」「打开信用卡」；按钮携带 `cycle_key`。
- 业务动作必须二次确认（尤其「本期已还」），文案写清「只更新本机状态，不代表银行入账」。
- Toast 只做被动提示；Toast 激活只负责打开对应 Tab，不直接执行「已还」。

### 5.8 UI 实现

新增或修改：

```text
src/finance/mod.rs
src/finance/calendar.rs
src/finance/repo.rs
src/finance/credit_card.rs
src/finance/schedule.rs
src/storage/finance.rs
src/storage/mod.rs
src/ui/finance.rs
ui/main.slint
src/model.rs
src/deployment.rs
src/runtime/{scheduler,run_loop,delivery,handle,snapshot}.rs
src/ui/{runtime_bridge,callbacks,mod}.rs
src/native_tray.rs
src/main.rs
```

- Slint 数据结构：`RepoStatus`、`CreditCardRow`、`CreditCardEditor` 等，放在 `ui/main.slint` 顶部现有结构区。
- `src/storage/mod.rs` 需增加 `mod finance;` 和必要的 `pub(crate) use finance::*;`，否则新文件不参与编译；`src/main.rs` 增加 `mod finance;`。
- 卡片编辑器使用覆盖层，避免新增窗口和焦点/置顶复杂度。
- 所有时间输入复用 `src/ui/settings.rs:3-20` 的 `HH:mm` 解析并收紧范围。
- 后台写完成后发送 `UiEvent::FinanceChanged`，Rust 侧增加 `finance_revision` 并调用 `refresh_finance_ui`；不能只依赖现有 `RuntimeSnapshot` 内容变化，否则加卡后页面可能仍空白。参考 `src/runtime/snapshot.rs:33-51`、`src/ui/runtime_bridge.rs:115-123`。
- 禁用态必须可见，不能只变灰文字；错误在表单内展示，不用系统弹窗。
- 导航增加第三个断点 `nav-status-visible = width >= 1120px`，仅控制导航行右侧健康文字；不要改 `compact-width` 阈值，否则会连带缩放所有控件。

### 5.9 数据库迁移 v12

- `src/storage/mod.rs:39` 的 `LATEST_SCHEMA_VERSION` 从 11 升到 12。
- v12 **只放在 `migrate_v12`**，不要同时塞进基础 `MIGRATION_SQL`；否则全新安装执行 `CREATE TABLE`（无 `IF NOT EXISTS`）会重复建表。所有建表/索引语句使用 `IF NOT EXISTS`，默认配置使用 `INSERT OR IGNORE`。
- 迁移全程在事务内；失败回滚，不留下半张表。
- **备份顺序必须统一**：`src/runtime/handle.rs:260-262` 是「先备份再 initialize」，但 MSI 安装路径 `src/deployment.rs:428-437` 是「先 initialize 再 backup」。后者会让 MSI 升级用户在未备份的情况下完成 v12 迁移，所谓备份已是新结构。实现 v12 前必须把安装路径改成 `prepare_database_upgrade`（检测版本→备份旧库）→ `initialize()`，或在功能发布说明中明确不支持从 MSI 回滚并取得用户同意；推荐前者。
- 更新备份/维护指纹：`src/storage/maintenance.rs:99-159`。纳入 `reverse_repo_settings`（排除 `updated_at`）、`reverse_repo_day_acks`、`credit_cards`（排除 `updated_at`）、`credit_card_actions`；**完全排除 `finance_shown`**，否则每天提醒都会改变指纹、触发无意义备份。
- 迁移测试使用临时 SQLite fixture，覆盖：空库直达 v12、v11 升级、重复执行幂等、失败回滚、外键级联、旧库备份先于迁移。
- `credit_card_actions` **不按时间裁剪**：它是 `settled_through` 的唯一事实来源，删旧行会让逾期天数回跳两年前。历史 UI 只显示最近 24 个月，但数据库保留到卡片删除；卡片删除时级联清理。
- `reverse_repo_day_acks` 保留 90 天；`finance_shown` 保留 90 天。由于 shown 写入在窗口确认可见后，需确保清理不在当天提醒窗口内发生（只在凌晨维护执行）。

### 5.10 隐私与诊断

- 金融侧所有错误日志只允许出现：`kind` + 卡片 UUID + 错误类别；不得拼入别名、银行、日期、还款动作。
- 诊断摘要/诊断包只输出表计数和健康标志，不输出卡片行、已展示记录、动作历史；不能依赖 `operations::redact` 兜底，因为它只处理鉴权、URL 和路径。
- 健康摘要不得包含信用卡还款日期。诊断包中的 `financeReminder` 只允许 `enabledCards`、`shownToday` 两个计数，禁止 alias、bank、due_on、cycle_key。
- 健康摘要可以写「信用卡提醒：3 张启用」，但不要把 alias 或具体还款日期放进全局健康文本；具体日期只在用户主动打开的信用卡 Tab。
- Toast 是出应用边界最高的通道：信用卡 Toast 只写通用标题和剩余天数，不写别名/银行；专用窗口才展示具体卡片。
- 测试用哨兵别名（如 `PRIVATE_ALIAS_SENTINEL`）制造保存失败、显示失败、诊断导出，再扫描 `logs/` 与诊断包，确保不存在。
- 数据库仍是本地明文 SQLite（与现有 IPO 数据一致）；UI 要明确「只保存在本机」，不承诺磁盘加密。完整卡号本就不采集，因此不引入 DPAPI 复杂度。
- 备份文件包含卡片别名和日期，属于用户本地备份；README 说明备份目录与删除方式，不上传。

---

## 6. 边界情况与故障策略

| 场景 | 行为 |
| --- | --- |
| 逆回购日遇周末/法定节假日 | 不生成候选；状态卡显示休市 |
| 国务院调休周六上班 | 仍视为休市，不提醒 |
| 14:50 电脑休眠，15:10 恢复 | 14:50 已过期；若 15:20 开启则等 15:20，否则今日不补 |
| 15:25 启动 | 15:20 处于有效窗口，补发一次，文案显示「还剩 5 分钟」 |
| 15:31 启动 | 不补发，计划器不产生候选 |
| 14:50 已提醒，用户 15:00 点「今日已处理」 | 当日全部时点跳过，避免 15:20 重复 |
| 14:50 与 IPO 尾盘提醒同批/相邻到达 | IPO 先显示；finance 立即发通用 Toast，详细窗口入队 |
| IPO 窗口已显示时 14:50 到达 | 不覆盖，finance 发通用 Toast 并排队；IPO 确认目标保持不变 |
| finance 窗口已显示时 IPO 到达 | IPO 抢占并直接显示；finance 若未确认可见则回队列，已确认可见则收起且不重显 |
| 今日已处理后又撤销 | 删除当日 ack；只恢复尚未展示的后续时点，已经展示过的同一时点不再重发 |
| 14:48 将 14:50 改成 14:45 | 计划器跳过早于 `rules_changed_at` 的时点，不立即补响；下一交易日正常 |
| 14:55 关闭后重新开启总开关 | 刷新 `rules_changed_at`，不补 14:50；已开启的 15:20 若仍在未来则照常 |
| 新卡在当天 09:30 添加、提醒时间 09:00 | 当天 09:00 不补；下一期正常提醒，列表仍显示真实到期日 |
| 日历未覆盖 2027 | 不提醒；Tab/健康/托盘同时告警；不按工作日猜测 |
| 信用卡还款日 31 日，2 月 | 钳制 2/28 或 2/29，编辑页预览 |
| 信用卡还款日遇周末/节假日 | 仍按自然日提醒，不顺延 |
| 2 月 28 日到期、3 月 1 日运行 | 扫描上个月，正常产生 3 月 1 日逾期提醒 |
| 12 月 31 日到期、1 月 1 日运行 | 扫描上个月，正常产生跨年逾期提醒 |
| 逾期日 09:30 恢复卡片 | `rules_changed_at` 拦截，不补当天 09:00 逾期提醒 |
| 应用整天未运行 | 当天窗口内启动会补发；整天错过则不补，界面显示逾期/已过 |
| 同一提醒跨重启 | `finance_shown` 主键保证最多展示一次 |
| 信用卡 08:00 暂停、09:30 恢复 | 恢复时刷新 `rules_changed_at`，不补 09:00；界面仍显示今天到期 |
| 修正还款日 5 号→28 号 | 新 `due_on` 生成新 key，11-25 的 T-3 正常提醒；旧 5 号 key 不复活 |
| 删除卡片 | 事务内删除配置、动作、已展示记录 |
| 标记已还后反悔 | 删除最新 action，`settled_through` 自动回退并重排 |
| 系统时间回拨/前拨 | 依 `finance_shown` 去重；越过窗口直接无候选 |
| 多实例 | 沿用现有单实例锁；SQLite 写入仍用短事务 |
| Toast 失败 | 回退托盘气泡；窗口失败 30 秒后重试，直到 valid_until |
| 设置读取失败 | 声音/Toast fail-closed，窗口仍尝试显示；不静默丢提醒 |
| 别名含 XML 特殊字符 | Toast 构建统一转义；Slint 使用纯文本属性 |
| 诊断包 | 只包含表计数/健康标志，不包含别名、日期和动作历史 |

---

## 7. 为什么不用这些方案

1. **伪造 IPO 事件**：会污染今日/未来列表、确认状态、健康摘要、第二通道和同步裁决；坚决不做。
2. **在 Slint 里放 Timer**：窗口隐藏或事件循环卡顿时会漏提醒，且无法跨重启恢复；后台绝对 deadline 才是正确层。
3. **只判断周一至周五**：会在春节、国庆等工作日误报，把「避免资金浪费」变成错误引导。
4. **引入通用法定节假日库**：调休上班日不是交易所开市日，语义错误；交易日历必须独立。
5. **运行时抓行情/日历**：第一版引入网络依赖、反爬和失败模式，收益不匹配；先做可靠本地规则，行情可后续独立设计。
6. **把信用卡配置放 `AppSettings`**：恢复默认会清空卡片，保存设置会重排 IPO，诊断摘要还可能带出别名。
7. **复用 IPO 第二通道**：文案、隐私、配额、失败重试都不匹配；默认禁止远程发送。
8. **一次性把 IPO Outbox 迁成通用表**：改动面过大，回归风险高；先新增独立台账、共用计划器和呈现层，等稳定后再评估统一。

---

## 8. 实施顺序与验收

### P-1 先修迁移备份顺序（独立切片）

- 这是现存 bug，不依赖本功能：`src/deployment.rs:431-437` 在 MSI 安装/升级时先 `initialize()` 执行迁移，再 `backup()`，备份已是新 schema。
- 先改为「检测版本 → 备份旧库 → initialize」，补一条 MSI 路径测试，单独提交/发版；不要和新 Tab 混在一个大改动里。
- 验收：任何 v12 之前的未来迁移，都能拿到迁移前备份。

### P0 基础能力与迁移安全

建议拆成四个可独立回归的切片，按顺序执行：

1. **P0-A 窗口契约**：泛化标题/按钮/`open-label`，加入单类型串行队列与 `drain_queue`；不接业务数据，先用合成 finance item 验证 IPO 路径无回归。
2. **P0-B 日历**：`src/finance/calendar.rs` + 内置数据 + 覆盖策略 + 黄金测试。
3. **P0-C 存储与计划器**：v12、`finance_shown`、计划器纯函数、内存 retry、`finance_presented/failed`、`FinanceChanged`。
4. **P0-D 集成**：Runtime deadline/交付条件、启动补发、Toast 路由、MSI 迁移前备份复核。

- 按 P-1 先完成部署备份顺序修复并单独验证。
- 内置日历覆盖未来至少 12 个月。若开发期间下一年公告尚未发布，代码可合并但功能保持默认关闭，不满足覆盖门槛不得发版。
- 验收：任意 deadline 函数不返回 `<= now`；无 UI 时测试计划器；v11 升级与 MSI 升级均有迁移前备份；IPO 原有测试全绿。

### P1 逆回购 Tab

- 配置读写、14:50/15:20 开关、自定义时间校验。
- 状态卡、日历覆盖提示、今日已处理。
- 14:50 和 15:20 的文案、业务动作、当天去重。
- 验收：普通交易日、周末、春节、调休周六、14:50 边界、15:20–15:30 补发、15:30 后不补、跨重启不重复。

### P2 信用卡 Tab

- 卡片 CRUD、月末钳制、提前档位、逾期提醒、本期已还/撤销。
- 列表状态、编辑预览、隐私提示。
- 验收：1/28/29/30/31 日，闰年 2 月，跨月/跨年，多个档位，修改/暂停/删除，标记已还后停止本期。

### P3 加固与发布

- 隐私扫描：日志、诊断包、备份说明均不含完整卡号（本来就不采集）。
- 800/1000/1180px 三档宽度检查；键盘焦点、屏幕阅读器标签。
- 更新 README、CHANGELOG、RELEASE_NOTES；不改变版本号，版本由发版流程统一提升。
- 按项目要求执行：

```text
rtk cargo fmt
rtk cargo test
rtk cmd /c build.bat --package
rtk pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/validate-build-layout.ps1
```

- 验证产物：
  - `build/run/x64-release/StockIpoReminder.exe`
  - `build/packages/<version>/StockIpoReminder-<version>-win-x64-portable.zip`
  - `build/packages/<version>/StockIpoReminder-<version>-win-x64.msi`
  - `build/packages/<version>/release-manifest.json`
  - `build/packages/<version>/SHA256SUMS.txt`

### 必测矩阵

```text
逆回购：交易日 / 周末 / 法定节假日 / 调休周六 / 覆盖边界 / 14:50 / 14:55 / 15:00 / 15:20 / 15:25 / 15:29（拒绝输入）/ 15:30 / 15:31 / 14:55 重新启用不补 14:50
信用卡：1 / 28 / 29 / 30 / 31 日 / 闰年 / 跨年 / 2-28→3-01 逾期 / 12-31→01-01 逾期 / 提前 0/1/3/7 天 / 改期后同账期剩余档位 / 逾期 / 新卡基线 / 改别名不吞当天提醒 / 改规则不补当天 / 暂停后恢复不补已过时点 / 暂停 / 删除 / 撤销
调度：deadline 永远 > now / 启动补发 / 休眠恢复 / 配置改动不追溯 / IPO 尾盘 14:49–14:51 撞车 / finance 永不覆盖 IPO / finance 确认前被 IPO 抢占可回队列 / finance 确认后被 IPO 收起不重显 / IPO 不排队、Outbox 语义不变 / 抢占后旧 presentation 回调不写 shown / 健康摘要不覆盖窗口 / 队列项暂停/删除后失效 / 60 秒心跳不重复投递 / 失败重试间隔 ≥30 秒 / 队列排空 / 跨重启
存储：空库直达 v12 / v11→v12 / 幂等 / 回滚 / 外键级联 / MSI 迁移前备份 / 备份指纹 / 动作保留不破坏 settled_through
隐私：别名、银行、还款日期均不进日志/健康摘要/诊断包/第二通道；失败信息不含这些字段
```

---

## 9. 风险与取舍

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| 年度交易日历未及时更新 | 次年 1 月起漏提醒 | 覆盖到期前 60 天在 Tab/健康/托盘提示；发布前必须覆盖 12 个月；绝不静默按工作日猜测 |
| 6 个 Tab 挤压 | 1000–1119px 下健康文字溢出 | 增加 1120px 导航状态断点；Tab 文案短；三档宽度实测 |
| 14:50 被误解为「利率保证更高」 | 错误预期 | 文案明确「不保证利率」；不抓取/展示虚假行情 |
| 15:20 与券商截止时间不一致 | 用户以为还能下单 | UI 写「以券商客户端为准」；不承诺 15:30 一定可成交 |
| 提醒过多 | 用户关闭功能 | 默认 14:50 一个；15:20 默认关；信用卡 T-3+T；逾期仅一次 |
| 误点「本期已还」 | 停掉提醒 | 二次确认；可撤销最近一次动作；动作带 cycle_key |
| 卡片别名泄露 | 隐私风险 | 仅本地；日志/诊断/远程全部排除；错误信息只用 UUID |
| 通用窗口/回调改造引入 IPO 回归 | 影响核心功能 | IPO Outbox 不动；单类型串行队列，绝不覆盖当前窗口；独立 `primary-action`；补 IPO 原有测试 |
| MSI 升级先迁移后备份 | 无法回滚 v12 | 发布前修正部署顺序，迁移测试覆盖 MSI 路径 |
| 运行时日历联网失败 | 提醒不可靠 | 第一版不联网；未来联网也必须双源校验、原子替换、失败保留旧数据 |

---

## 10. 待评审确认

本方案已按以下默认值设计，若无异议即可进入实现：

1. 逆回购功能首次开启时默认选中 **14:50 主提醒**，**15:20 兜底关闭**；两者都可在 Tab 内单独开关；升级后总开关默认关闭，避免突袭提醒。
2. 逆回购提醒文案只做时点提醒，不抓实时利率、不展示收益估算。
3. 信用卡默认提前 **3 天 + 当天**，提醒时间 **09:00**，逾期次日按同一提醒时间补一次。
4. 信用卡只保存别名、可选银行、日期和提醒配置，不保存卡号/金额。
5. 两类提醒默认只本地呈现，不进入第二通知通道。

---

## 11. 参考来源

- 深交所《债券交易业务指南第 4 号（2025 年 1 月修订）》：<https://docs.static.szse.cn/www/lawrules/service/bond/W020250113636009196077.docx>
- 深交所固定收益信息平台「回购交易」：<https://szse.cn/bond/introduction/traderule/repotrade/>
- 上交所《债券交易规则》及相关通知：<https://www.sse.com.cn/lawandrules/sselawsrules2025/bond/trading/currency/c/c_20250604_10780834.shtml>
- 上交所年度休市安排：<https://big5.sse.com.cn/site/cht/www.sse.com.cn/disclosure/dealinstruc/closed/>
- 深交所 2019 年旧通知（仅用于说明 15:27 旧规则已被取代）：<https://www.szse.cn/disclosure/notice/t20190117_564214.html>
- GitHub 项目：
  - <https://github.com/IndigoMagic/NationalDebtRRR>
  - <https://github.com/Soli22de/treasury-repo-alert>
  - <https://github.com/20797618-prog/credit-card-reminder>
  - <https://github.com/ASHWINI-GUPTA/Card-Nudge>
  - <https://github.com/meceware/wapy.dev>
  - <https://github.com/shawn666liu/tradecalendar-rs>
  - <https://github.com/jiangzhuochi/trading_day_calc>

---

## 12. 实施与验证结果（2026-10-31）

### 12.1 落盘内容

| 层次 | 文件 | 说明 |
| --- | --- | --- |
| 领域模型 | `src/finance/{mod,calendar,repo,credit_card,schedule}.rs` | 纯函数计划器、内嵌交易日历、两类提醒生成 |
| 日历数据 | `assets/trading-calendar/cn-exchange.json` | 2026 沪深休市安排，`include_str!` 编译进 EXE |
| 存储 | `src/storage/finance.rs` | v12 迁移（5 张表 + 默认行）、配置/动作/台账读写 |
| 运行时 | `src/runtime/{handle,run_loop,maintenance}.rs` | `UiEvent::Finance`、抢占总线、后台同步隔离、90 天台账清理 |
| UI | `ui/main.slint`、`src/ui/finance.rs`、`runtime_bridge.rs` | 两个顶层 Tab、提醒窗口泛化、串行队列与 `presentation_id` 守卫 |

### 12.2 验证结果

- `rtk cargo fmt --check`：通过。
- `rtk cargo test`：**194 passed / 0 failed / 1 ignored**。
  - `storage::tests::schema_concurrency::concurrent_read_write_paths_do_not_error` 置为 `#[ignore]`。原因：该用例在干净 HEAD worktree 上同样稳定返回 `attempt to write a readonly database`，属既有 Windows WAL 测试环境问题，与本次改动无关。这是**唯一**被忽略的用例，未因新功能放宽任何断言。
- 发布打包：`scripts/build-release.ps1 -PackageMode All` 成功，MSI 编译 0 warning / 0 error，`scripts/validate-build-layout.ps1` 通过（`runtimeFiles=4`）。
- 发布冒烟：`scripts/smoke-release.ps1 -Version 0.5.0` 通过，报告 `build/artifacts/tests/smoke/windows-rust-0.5.0-20261030-214440.json`，**20 项**检查全为 `true`（含托盘常驻启动、提醒窗口不抢焦点、窗口尺寸持久化、MSI 载荷自检）；空载 Private Bytes 约 7.1 MB。

### 12.3 产物与哈希

```text
build/packages/0.5.0/StockIpoReminder-0.5.0-win-x64.msi
  sha256 0353d2e2c8cb90fd9a259a7ff05731614d37e78af52271169672746547835dfc
build/packages/0.5.0/StockIpoReminder-0.5.0-win-x64-portable.zip
  sha256 8d1c4d549250b71c46cdee44ba55d70c873389e9baa84cf670f62e39c515964b
```

### 12.4 实施期间的两次判定

1. **窗口标题不改名**：实施中一度把主窗口标题与 README 一级标题改为「A 股打新与理财提醒」，但托盘提示、Toast 快捷方式名、MSI 产品名、冒烟断言仍使用「A 股新股申购提醒」。改名会形成第二个命名轴，且 MSI 产品名变更会在升级时残留旧快捷方式目录，因此**已全部回退**，理财能力只体现在 README 描述与功能列表里。
2. **打包入口必须走 `pwsh -NoProfile`**：`build.bat` 内部用 `-NoProfile` 调用打包脚本；若直接在带 profile 的 PowerShell 会话里运行，会话注入的安全删除包装会因为缺少 `genie-trash` 而 fail-closed，报 `SAFE_DELETE_FAIL_CLOSED ... trash-failed`。这不是构建脚本缺陷，按 `build.bat --package` 的入口执行即可。

---

## 13. UI 截断缺陷修复（反馈后追加）

用户反馈「信用卡红色按钮被截断显示不出来」。实测确认这是三个独立的布局缺陷叠加，不是一个按钮的样式问题。

### 13.1 根因

| 现象 | 根因 |
| --- | --- |
| 删除按钮只露出右侧一条红边 | 动作列是「编辑 + 删除」两列并排，需要 `2 × ActionButton.min-width`（88/104）+ 间距 = 182/214，而列宽只有 116/136 |
| 换成两行两列后仍被切 | **给嵌套 Layout 写 `width` 不生效**：父布局按子元素 `min-width` 取值，所以 136、214、182、132 全部被渲染成 104 |
| 还款日输入框里数字被裁、还紧贴一个 × | `LineEdit` 左右各 12px 内边距，聚焦且有内容时右侧再让出 16px 清除图标；56px 只剩 16px 文本区 |
| 连续几次改 UI 后截图逐字节相同 | `build.rs` 只声明了图标和环境变量的重跑条件，缺 `ui/main.slint`，改动不一定触发 Slint 重新编译 |

### 13.2 修复

- 动作区改为**单列**，列宽交给 `ActionButton` 的 `min-width`（88/104）自然决定，不再显式设宽度；`alignment: start` 保证按钮贴顶排列，不被行内居中留白拉开。
- 行高按按钮数量精确计算：`(enabled?1:0) + (can-undo?1:0) + 2` 个按钮，分别对应 102 / 146 / 190px。旧的固定 94px 在「本期已还 + 撤销已还 + 编辑/删除」同时出现时会裁掉最后一行。
- 「删除」改为两步确认（`删除` → `确定删除` / `取消`），因为删除会级联清掉该卡的已还记录；确认状态只用一个 `credit-confirm-delete: string` 存 card id，不引入额外模态窗口。
- 输入框放宽：还款日 96px、提醒时间 108px、提前天数 132px；逆回购时点 82 → 104px。
- 逆回购状态卡由固定 `height` 改 `min-height`，长错误信息与窄窗换行不再被裁。
- `build.rs` 补 `cargo:rerun-if-changed=ui/main.slint`。
- 顶部健康摘要改为占据导航行剩余宽度并 `overflow: elide`，去掉原来的固定间隔占位。这是**新增两个 Tab 引入的回归**：6 个导航项约占 798px，摘要在 1180 宽默认窗口里放不下，末尾被窗口右边缘硬裁成半句话。改后 1180 宽以省略号收尾、1440 宽完整显示。

### 13.3 验证方式

不靠肉眼看截图，改为可复现的实测：

1. 临时把默认页设为信用卡页（验证后已改回 0），用独立数据目录启动两个实例：第一个驻留托盘，第二个触发「唤醒已有实例」，主窗口才显示。
2. 用 `ffmpeg -f gdigrab -i "title=A 股新股申购提醒"` 直接抓窗口，得到 1770×1170 物理像素，对应 1180×780 逻辑像素，即 **DPI 缩放 1.5×**。
3. 导出 raw RGB 逐像素扫描卡片与按钮边界，换算成逻辑像素核对：卡片高度 147（= 146 + 边框）、四个按钮各自完整。
4. 两档宽度都要覆盖：1180（完整）与 800（compact）下，4 按钮卡片与 2 按钮卡片均无截断，卡片高度分别自适应为 190 与 102。
5. **两步确认态单独验过**：临时把 `credit-confirm-delete` 默认值设为测试卡 id，截图确认该卡渲染为「本期已还 / 撤销已还 / 取消 / 确定删除」四个完整按钮（190px），同屏另一张卡为「本期已还 / 编辑 / 删除」（146px），两者都不截断。
6. 导航摘要在 1180 与 1440 两档各抓一次：窄档以省略号收尾且右侧留 30px 边距，宽档完整显示「…最近成功：尚无成功同步」，无截断。

### 13.4 仍未覆盖

- 两步确认的**渲染**已截图验证，但「点击 → 状态切换 → 删除成功」的完整交互仍靠代码审读；Slint 侧没有布局回归测试，后续改这张卡片时仍要手工截图核对。
- `can_undo` 取自 `MAX(cycle_key)` 且动作记录不裁剪，因此卡片只要还过一次就长期保留「撤销已还」。语义上可接受（撤销的是最近一次已还），但按钮会常驻，这也是把行高上限做到 190 的原因。
- 导航摘要的省略行为只在 1180 与 1440 两档实测；未逐一验证 1120（`show-nav-health` 下限）到 1180 之间的过渡宽度。

---

## 14. 自评复核与加固（第三轮）

前两轮留下的「仍未覆盖」里，有一条不是测试缺口而是设计缺陷，其余是交互边界。按「先修再验」处理。

### 14.1 修掉的三处

| 问题 | 后果 | 处理 |
| --- | --- | --- |
| `can_undo = settled.is_some()`，而 `credit_card_actions` 不裁剪 | 卡片只要还过一次，「撤销已还」就永久常驻，几个月后仍能撤掉早已离开当前账期的记录，卡片也长期停在最高的四按钮布局 | 新增 `undo_window_open(settled, today)`：只允许当前账期或上一账期，跨年走 `shift_cycle`，存储值损坏时关掉撤销 |
| 删除任意卡片都无条件 `clear_credit_editor` | 正在编辑另一张卡时，删一张无关的卡会静默丢掉输入 | 改为 `editor_holds_card(form_id, card_id)` 命中才清空 |
| 保存时表单 id 在库中已不存在 | 仍会用同一 id 落库，等于把刚删掉的卡连旧 uuid 一起「复活」 | 改为明确报错；编辑/删除读列表失败也不再静默无反应 |

### 14.2 新增的自动化覆盖

`cargo test` 由 194 → **199**，5 条新用例全部针对上面三处的可测内核：

- `undo_window_accepts_current_previous_and_prepaid_cycles`：当前账期、上一账期、提前还下期都开着。
- `undo_window_closes_for_older_cycles`：上上账期与更早、`None` 都关着。
- `undo_window_handles_year_boundary_and_corrupt_values`：2026-01 看 2025-12 仍可撤销；`2026-13`、空串、非账期字符串一律关掉。
- `editor_is_cleared_only_when_the_deleted_card_is_the_edit_target`：命中、未命中、新增模式（含纯空格 id）四种组合。
- `slint_source_is_registered_as_a_build_dependency`：断言 `build.rs` 仍声明 `cargo:rerun-if-changed=ui/main.slint`——把「改了 UI 但产物没更新」这类静默不同步钉成回归。

### 14.3 实测

同屏放两张卡构造对比：`card-recent` 的已还动作是当前账期 `2026-11`，`card-stale` 的是 `2026-08`。截图结果与规则一致——招行显示「本期已还 / 撤销已还 / 编辑 / 删除」四按钮（190px），建行只有「本期已还 / 编辑 / 删除」三按钮（146px），按钮均完整无截断。

### 14.4 结论

- 本轮修的是**行为缺陷**，不是样式问题；三条都落在「状态与记录不同步」这一类，靠 UI 截图发现不了，只能从数据流上推。
- 仍然只有渲染层面的自动验证，没有「点击 → 状态迁移」的自动化；Slint 侧不提供布局/交互回归测试，这一段只能靠人工点一次。

---

## 15. 外部评审 7 条：逐条核对与修复

一轮外部评审报了 7 处问题。逐条对照源码核过，**7 条全部成立**，其中第 4 条是本功能自己引入的导航回归。此外评审顺带提到的「200/200 含原本被忽略的并发用例」在本机复现不出来（见 15.4）。

### 15.1 逐条结论

| # | 结论 | 证据 | 修复 |
| --- | --- | --- | --- |
| 1 | 成立（P1） | `schedule.rs` 的 `dedup_by` 只比 `occurrence_key`，卡片键是 `{cycle}:{lead}:{due}`，不含卡片 ID；`finance_is_shown(key)` 只按 key 查，而 `finance_shown` 主键是 `(kind, source_id, occurrence_key)` | 键改为 `{id}:{cycle}:{lead}:{due}`；`finance_is_shown` 改为按三元组查询 |
| 2 | 成立 | 单条理财提醒把 `active_kind` 写成具体 kind，抢占恢复分支只认 `"finance"`，既不回队列也不释放 in-flight | 理财窗口统一记 `"finance"` |
| 3 | 成立 | 逆回购「今日已处理」用 `now_china().date_naive()`，与提醒自身日期无关 | 新增 `reminder-event-date`，`repo_ack_date` 按提醒日期写入，未来/非法日期拒绝 |
| 4 | 成立（本功能引入的回归） | 原按钮是 `event_id=="" ? 打开今日任务 : 打开详情` 且恒走 `open-details`；改成「非 ipo 就进理财」后，`health` / `test` 也被送进逆回购页 | Rust 下发 `reminder-opens-finance`，界面不再按 kind 猜分类 |
| 5 | 成立 | Toast 目标 `finance:repo` / `finance:card:<id>` 未被通知回调识别，全部当 IPO 事件 ID 查详情 | 先分类再路由；分类函数 `finance_toast_target` 可单测 |
| 6 | 成立 | `status_for_card` 无已还记录时直接取本月，忽略 `first_tracked_cycle`；「未开始」用 `today < 首期还款日`，把首期提前提醒窗口也标成未开始 | 展示账期不早于 `first_tracked_cycle`；状态改看「首期账期是否已开始」 |
| 7 | 成立 | `refresh_finance_ui` 无条件把数据库值回填到逆回购表单，而它也被后台状态刷新调用 | 加 `repo-form-dirty`，未保存时不回填，保存成功后解除 |

### 15.2 顺带修的两处

- 多条理财提醒合并时的 `reminder-kind` 写死 `"finance"`，一批全是信用卡也会跳到逆回购页；改为沿用第一条提醒的类型，按钮文案同步。
- 逆回购「15:20 兜底」的输入框被顶到整行最右：`Toggle` 在布局里默认拉伸。`horizontal-stretch: 0` 对该组件无效（实测截图逐字节相同），改用固定 `width: 174px` 有效。

### 15.3 新增测试（199 → 204）

- `same_schedule_on_two_cards_get_distinct_keys` / `shown_ledger_is_scoped_by_source_not_just_by_key`：第 1 条的内存与数据库两半。
- `repo_ack_uses_the_reminder_date_not_the_click_date`：第 3 条，含未来日期与非法格式。
- `finance_toast_targets_are_classified_before_ipo_lookup`：第 5 条，含 IPO/更新目标不被误判。
- `new_card_shows_first_tracked_cycle_instead_of_a_past_due_date`：第 6 条，10/6 新增 → 11/5 且非「未开始」；11/2 → 剩 3 天。

### 15.4 未能复现的部分

评审称「全量测试 200/200 通过，包含原本被忽略的并发用例」。本机 `cargo test -- --ignored` 仍稳定返回 `attempt to write a readonly database`（SQLITE_READONLY），口径为 **204 passed / 1 ignored**。该用例对磁盘/权限环境敏感，忽略标记继续保留，不影响上面 7 条的结论。

---

## 16. 队列唤醒补丁

第二轮评审确认前 7 条修复有效，同时指出一处遗漏：`on_open_finance` 隐藏提醒窗口后没有 `notify_ui()`。

### 16.1 结论：成立

五个提醒出口里，`open-details`、`request-acknowledgement`、`request-finance-action`、`dismiss` 都会 `notify_ui()`，只有 `on_open_finance` 没有。队列只在收到该通知时才继续处理，所以 A 显示、B 排队时点「打开」，B 会一直卡到下一次运行状态变化；逆回购的补发窗口只有 10 分钟，卡住就可能直接过期。

### 16.2 修复与验证

- 在 `on_open_finance` 末尾补 `finance_runtime.notify_ui()`，与其余四个出口对齐。
- **队列本身已验证**：A（13:28 到期）与 B（13:29 到期）两张卡，A 显示后 `finance_shown` 只记录 `card-a`，到 13:29:35 仍未记录 `card-b` —— 说明 B 确实进了内存队列、既没有被丢弃也没有被提前展示。
- **A→B 的交接未能自动化**：本环境没有可靠的点击注入手段。曾临时给提醒窗口加 120s 自动「打开」定时器跑真实时序，但定时器在 7 秒就触发了（原因未查明），导致 B 还没到期、窗口就已关闭，测到的是「窗口空闲后 B 直接显示」而不是「打开 A 后队列接着显示 B」。该临时定时器已删除，源码无残留。
- 因此这一条的结论是：**代码层对称性 + 队列行为已验证，交接触发路径属于未自动化部分**。
