# 变更日志

本文件按版本倒序记录本仓库的变更，用于回答「改了什么、什么时候改的、影响面在哪」。

## 维护约定

- 每次源码、UI、脚本、打包或发布流程变更后，先在 `[未发布]` 小节登记，说明变更类型（新增 / 变更 / 修复 / 移除 / 安全）和影响面。
- 版本定稿并跑完 `rtk cargo fmt`、`rtk cargo test`、`build.bat --package`、布局校验后，把 `[未发布]` 小节改名为 `## <版本> — <日期>`，再另起一个空的 `[未发布]` 小节。
- 版本号只以 `Cargo.toml` 为准，本文件不重复维护第二个版本来源。
- `RELEASE_NOTES.md` 面向用户，记录「用户能感知到什么」；本文件面向维护者，额外记录内部重构、脚本和测试等工程变更。
- 只追加、不改写历史条目；历史版本如确需更正，追加说明而不是覆盖原文。

---

## [未发布]

### 修复

- **还款条目更紧凑且删除按钮完整可见**：移除信息列与动作组嵌套 Box 的默认内边距，常用操作改为两行两列；普通条目从 128/172px 收紧为 100px，同时可还款、可撤销的贷款条目为 144px。删除与结清的二次确认在原位替换底行，避免出现三按钮挤在同一行或重复的删除确认。
- 新增真实 Slint 软件渲染与点击回归测试，覆盖 800×500 最小窗口、1180×780 常规窗口，以及贷款、信用卡、可撤销、已结清和停用账户的完整删除按钮与二次确认。

### 验证

- `rtk cargo fmt -- --check`、`rtk cargo test` 通过：248 passed / 1 ignored；忽略项为既有 Windows WAL 并发环境测试。
- `rtk cmd /c build.bat --package`、布局校验、发布包 SHA-256 与运行目录 EXE 一致性校验通过；Rust 与 MSI 构建无警告。`scripts/smoke-release.ps1 -Version 0.6.4` 的 20 项检查全部通过，包含便携启动、MSI 管理解包与载荷自检（报告：`build/artifacts/tests/smoke/windows-rust-0.6.4-20261111-144118.json`）。未执行机器级 MSI 安装/卸载。

## 0.6.4 — 2026-11-11

### 修复

- **升级后不再把旧账单归入升级当月**：schema v16 保留信用卡金额备注与已还记录，将 v15 无法区分来源的金额账期转为待确认；只有重新输入金额并保存后才按下一待还账期计入汇总，普通编辑别名不会确认账期。贷款月供仍长期有效。
- **已还金额按账期保存快照**：四类还款账户在标记已还的同一事务中保存金额，月度已还汇总与卡片读取快照；提前录入下期账单、调整月供或重复点击已还不会改写本月已还金额。撤销会同时撤销该账期快照，零金额与未知金额分别保存；旧已还记录不猜测历史金额。
- **金额预览随重新填写更新**：重新输入有效金额后取消旧账期提示；尚未确认的旧金额不再拼接成下一期待还金额，列表显示具体账期或「账期待确认」。
- **读取还款记录失败时停止刷新汇总**：读取已还进度或金额失败会在状态栏报错，保留原界面，避免当成未还或零金额展示；删除汇总中不可达的重复逾期分支。

### 验证

- 新增 v14/v15→v16 数据保留、迁移失败事务回滚及重试、金额快照与重复动作/撤销、信用卡账期确认和真实 Slint 表单回调回归测试；金额账期和快照均纳入备份指纹与备份数据校验。
- `rtk cargo fmt -- --check`、发布流程内 `cargo test`：247 passed / 1 ignored；忽略项为既有 Windows WAL 并发环境测试。Rust 编译无警告，MSI 构建 0 warning / 0 error。
- `rtk cmd /c build.bat --package`、`scripts/validate-build-layout.ps1` 及包文件 SHA-256 校验通过；`scripts/smoke-release.ps1 -Version 0.6.4` 的 20 项检查全部通过，包含便携启动、提醒窗口、单实例、尺寸记忆及 MSI 管理解包/载荷自检（报告：`build/artifacts/tests/smoke/windows-rust-0.6.4-20261111-124958.json`）。未执行机器级 MSI 安装/卸载或 Windows 10 实机复核。

## 0.6.3 — 2026-11-11

### 修复

- **信用贷提醒弹窗的「本期已还」与「打开还款提醒」**：`credit_loan` 现在与信用卡、房贷、车贷走同一条分支，可正常标记当期已还、跳转还款页并高亮账户，不再报“不支持的理财提醒类型”，也不会误跳逆回购页。判定逻辑收敛为 `is_payment_reminder_kind` / `opens_payment_page` 两个函数，避免四处手写类型列表再次漏项。
- **修改金额不再取消当天待展示的提醒**：还款金额与借款本金只是备注数据，保存时不再刷新 `rules_changed_at`。此前 09:00 已到点排队、09:05 修改金额会让调度按新的规则时间跳过该提醒，导致提醒直接不出现。
- **信用卡「本期金额」绑定账期**：SQLite schema v15 为 `credit_cards` 增加 `amount_cycle`，信用卡金额在填写或重新填写时绑定到当时的下一个待还账期；跨月后旧账单不再计入当月待还/已还汇总，列表显示「本期待填写 · 上期 …」，编辑表单提示旧金额属于历史账期。贷款月供不绑定账期，继续长期沿用；升级时按升级当月回填已有信用卡金额的账期。
- **逾期提示优先于「本月已全部结清」**：本月范围内的账户即使全部已还，只要仍有旧账逾期，顶部汇总也先展示逾期项数和本月已还金额，不再被“已全部结清”覆盖。
- **编辑表单的下一期预览按已还进度计算**：预览改为复用 `status_for_account` 与表单里修改后的还款日/结束月，首期已还后显示下一待还账期；结束还款月已过时显示「还款周期已结束」。

### 验证

- `rtk cargo fmt`、`rtk cargo test`：242 passed / 1 ignored。
- 新增/调整覆盖：四类还款提醒的类型判定、四类账户写入已还记录、金额编辑保持 `rules_changed_at` 不变、信用卡金额跨月失效而贷款月供长期有效、逾期与本月已还并存的汇总文案、schema v15 迁移。

## 0.6.2 — 2026-11-10

### 新增

- **扩展支持「信用贷」账户类型**：在信用卡、房贷、车贷基础上增加第四类还款账户「信用贷」（属于贷款类型，支持设置结束还款月、标记已结清及恢复）；配套设计专属矢量徽标 `assets/banks/kind_credit_loan.svg`（琥珀暖橙几何盾徽造型，零外部依赖）。
- **每期还款金额与借款本金备注**：还款表单新增「每期金额/月供」选填项（支持信用卡当期应还及房贷/车贷/信用贷每期月供，支持千分位与货币符号），贷款类额外支持备注「借款总额/本金」；列表第一列清晰区分待还、已还、结清金额状态。
- **当月应还总额智能目测汇总**：页面顶部状态栏新增总额目测计算，聚合当月待还金额、已还金额与当月应还总额（支持展示部分未填金额项计数），一眼掌握当月全部现金支出压力。
- **SQLite 存储迁移 v14**：数据库 `credit_cards` 表新增 `amount` 与 `total_amount` 双精度浮点列，无缝保留历史记录并参与业务指纹比对。
- **官方风格矢量银行图标**：重构 `assets/banks/` 下 21 家主流商业银行图标为现代圆角矩形纯几何矢量 SVG，遵循各行官方品牌色，移除对系统字体依赖，完全编译嵌入 EXE。
- **三类账户语义化降级图标**：新增 `kind_credit_card.svg`（卡片与芯片）、`kind_mortgage.svg`（房屋剪影）、`kind_auto_loan.svg`（汽车轮廓）及 `bank_building.svg`（古典金融神殿建筑），未指定银行时按账户类型精准降级。
- **自定义银行动态微徽标生成**：`src/finance/bank.rs` 新增 `generate_monogram_svg` 与 `account_icon_svg`，用户在「其他银行」键入自定义名称时，按确定性字符串哈希和调色板动态生成双字纯矢量微徽标，并在表单编辑时实时响应。
- **智能别名预填建议**：在表单新增账户时，选择银行及账户类型会自动联动建议别名（如「招商银行 信用卡」），已自定义别名时不覆盖。
- **扩充银行简称字典**：补充「工银」「建融」「建银」「交银」「BOCOM」「招银」「浦银」「民银」「兴银」「PAB」「GDB」「邮储银」等高频民间简称与别名映射，并添加唯一性防冲突测试。

### 修复

- **自定义银行名称 XML 特殊字符转义**：修复 `generate_monogram_svg` 在用户输入包含 `&`、`<`、`>`、`"` 或 `'` 时破坏 SVG XML 结构导致图像解析失效的漏洞，增加 `escape_xml_text` 与特殊字符测试。
- **表单编辑手动别名被覆盖问题**：增加 `credit_form_alias_manual` 状态位，一旦用户手动编辑过别名或处于编辑模式，切换类型与银行均不再自动重填冲掉用户输入；清空表单后恢复建议联动。

### 验证

- `rtk cargo fmt`、`rtk cargo test`：240 passed / 1 ignored，覆盖信用贷枚举属性、金额范围与货币格式化、v14 SQLite 迁移、变更追踪、金额输入解析、月度汇总分支及 UI 图标注册测试。
- 资源核验：全套 SVG 资源总增量 < 20KB，断网便携版与安装版零外部依赖。

## 0.6.1 — 2026-11-09

### 修复

- **还款提醒页新增多个账户后无法滚动**：折叠表单仍会被页面剩余高度纵向拉开，与列表并列时把 `ListView` 压成只剩一条卡片高；账户增多后无法访问后面的账户。现在表单与列表互斥显示：表单展开时独占可用高度并由内部 `ScrollView` 滚动，收起后显示占满剩余高度的账户 `ListView`。页面高度按窗口高度计算，避免 `ScrollView` 在根布局里按内容撑出窗口、看似有页面却没有视口。
- **列表操作失败提示不可见**：表单收起后原来的错误文本也随之隐藏，删除、已还、撤销、结清、恢复或读取失败时用户看不到原因。这些错误现在同时写入主窗口状态栏。
- **银行选择索引失效时静默改为未指定**：保存前若下拉模型已变化、当前索引越界，现在会拒绝保存并提示重选，不再把银行静默清空。
- **增加还款页布局回归断言**：`src/ui/tests.rs` 新增 `repayment_form_and_account_list_never_share_the_vertical_viewport`，锁定表单与列表互斥、表单/列表高度来源和 `ListView` 结构。

### 变更

- **诊断包 schema 升到 v5**：`financeReminder.enabledCards` 改为 `enabledAccounts`（语义已从信用卡扩展到三类还款账户），同步提升诊断摘要 schema，避免消费方按旧字段误读。
- **提醒窗口“打开”按钮按批次实际目标生成**：混合批次首条若是逆回购、实际却跳转还款页时，不再显示错误的“打开逆回购”。

### 验证

- `rtk cargo fmt`、`rtk cargo test`：228 passed / 1 ignored。
- 真实 UI 自动操作复核：连续新增两个账户均成功落库；800×500 最小窗口下表单内部可滚动，收起后账户列表可独立滚动并访问第二张卡；横向无裁切。
- `scripts/build-release.ps1 -PackageMode All`、`scripts/validate-build-layout.ps1` 与 `scripts/smoke-release.ps1 -Version 0.6.1` 通过，20 项发布检查全部为真（报告：`build/artifacts/tests/smoke/windows-rust-0.6.1-20261109-150411.json`）。

## 0.6.0 — 2026-11-09

### 新增

- **统一还款提醒账户**：`CreditCard` 领域模型泛化为 `PaymentAccount`，支持信用卡、房贷、车贷；保留 `credit_cards` / `credit_card_actions` 物理表名，SQLite schema 从 v12 升至 v13，新增 `account_type`、`bank_id`、`final_cycle`、`closed_at`。旧账户 UUID、银行原文、首账期、规则时间、已还动作和 `finance_shown` 台账不丢失。
- **贷款结束与结清**：房贷、车贷支持包含端点的结束还款月和人工结清/恢复。调度按「已结清 → 暂停 → 超过结束月」顺序终止；结束月已过但仍有未处理账期时继续显示该账期，全部处理后才显示周期结束。恢复会启用账户并刷新规则时间，不补发旧时点。
- **银行注册表与本地图标**：新增 21 家银行加「其他银行」的稳定 ID/显示名/精确别名注册表；22 个本地 SVG 徽章通过 `include_bytes!` 编译进 EXE，另有未知银行 fallback。旧手填银行名只在登记名称或别名精确匹配时映射，未命中归入「其他银行」并保留原文；未知未来 ID 编辑时不会被静默改成未指定。
- **表单与列表交互**：新增/编辑表单可折叠，银行和还款日使用下拉，提前提醒提供常用预设并保留自定义入口，贷款条件显示结束还款月；列表增加银行图标、类型、结束月、结清/恢复动作。

### 变更

- **提醒身份改为三元组**：`(kind, source_id, occurrence_key)` 贯穿调度聚合、运行时 in-flight、失败重试、会话内去重、有效性复核和排队丢弃；旧信用卡 `occurrence_key` 格式保持不变，升级后不会因新类型或复合身份重复展示。
- **通知与路由泛化**：`FinanceKind` 增加房贷/车贷；三类账户的提醒主操作统一进入已还存储，未知类型返回明确错误而非静默成功。Toast 路由使用 `finance:payment:<id>` 并兼容 `finance:card:<id>`，单条可定位账户，多条或混合批次只打开统一页面。
- **存储层不变量**：结束月不得早于首账期或最高已还账期；信用卡禁止结束月/结清；账户类型不可变；已还账期必须严格 `YYYY-MM`、不早于首账期、不晚于结束月；已结清账户禁止新增已还。撤销仍在存储层复核窗口，并保留当前允许最高未来已还账期的兼容行为。
- **备份与诊断**：新字段纳入业务备份指纹；诊断仅输出泛化启用账户数，不包含别名、银行、结束月或具体日期。v12 升级测试使用真实旧表结构和旧数据，不在 v13 库上删版本记录模拟。

### 修复

- **窄窗口按钮被裁切**：还款列表按滚动条占位增加右侧内边距，动作区改为最多三行、每行最多两个按钮，并重算 128/172px 行高；800px 宽和高 DPI 截图复核不再出现删除按钮被卡片右边缘或滚动条截断。
- **表单被纵向拉伸**：折叠表单在根 VerticalBox 中显式 `vertical-stretch: 0`，字段保持紧凑，不再把开关和按钮拉出大段空白。
- **原有计划事实纠偏**：撤销窗口实际允许当前、上一及未来已还账期，实施没有按计划概括误收窄；保存失败时表单保持展开并保留输入。

### 验证

- `rtk cargo fmt` 通过；`rtk cargo test` 226 passed / 1 ignored，新增 v12→v13 迁移、贷款终止/结清/恢复、结束月历史冲突、未知银行保留、跨类型提醒身份和备份指纹测试；发布构建与测试构建均无编译警告。
- GUI 复核：隔离数据目录、150% DPI 下截取还款提醒页、折叠/展开表单及 800px 紧凑宽度，确认银行图标、列表动作、右侧删除按钮和表单字段完整可见。
- `scripts/build-release.ps1 -PackageMode All` 与 `scripts/validate-build-layout.ps1` 通过，MSI 编译成功；`scripts/smoke-release.ps1 -Version 0.6.0` 的 20 项发布检查全部为真（报告：`build/artifacts/tests/smoke/windows-rust-0.6.0-20261109-135753.json`）。

## 0.5.1 — 2026-11-03

### 修复

- **切到「逆回购」页签会把主窗口缩小并锁死尺寸**：逆回购页「提醒时段」的时点行 4 个控件全部用 `width:` 固定宽（Slint 把显式 `width` 同时当作 min 和 max），行 max 经 VerticalBox「max 取最小」聚合把整页 max-width 钉死在约 654px；Slint winit 后端把根布局约束当作窗口 min/max 尺寸，切页触发 `adjust_window_size_to_satisfy_constraints` 强制把窗口钳进约 654–800px 宽，`set_max_inner_size` 又禁止拉宽，停留 1.5 秒后被钳制的尺寸还会覆盖 `window-state.json` 里的用户尺寸。信用卡/设置页的固定宽控件行都有无界 Text 兄弟（行 max 无界）所以不受影响。修复：给 `MainWindow` 显式设置 `max-width/max-height: 10000px`（与 `window_state.rs` 的 `MAX_LOGICAL_DIMENSION` 一致），根元素显式约束覆盖布局汇总的 max 字段，窗口尺寸重新由用户和尺寸记忆决定；逆回购行的固定宽布局保持不变。

### 变更

- **给主窗口尺寸约束加回归断言**：`src/ui/tests.rs` 新增 `main_window_declares_window_level_max_size`，断言 `MainWindow` 仍声明窗口级 `max-width/max-height`。Slint 会把页面布局汇总出的 max 当成窗口上限，这两行一旦被删就会退回「切逆回购页锁窗口」的老问题，`cargo test` 与发布冒烟都覆盖不到。
- **发版文档对齐 `RELEASE_NOTES.md` 的 `[未发布]` 写法**：`docs/release-signing-and-updates.md` 的发版顺序与一致性自检改为「把 `[未发布]` 改名为本版本小节」，并明确发版后不留空占位、打包进 `build/packages/<版本>/` 的副本不得出现 `[未发布]` 标题（该文件原样发给用户）。

### 验证

- `rtk cargo fmt -- --check` 通过；`rtk cargo test` 205 passed / 1 ignored（新增 `ui::tests::main_window_declares_window_level_max_size`，删掉 `max-width` 那行会失败，已做反向验证）。
- GUI 复核（隔离数据目录 + 150% DPI + Win32 `SetWindowPos`）：1600×1000 下切「逆回购」「信用卡」窗口尺寸不变；停在逆回购页可继续拉到 1750×1100；截图确认两页内容铺满宽度、无裁切或控件重叠。
- `scripts/build-release.ps1 -PackageMode All` 与 `scripts/validate-build-layout.ps1` 通过，MSI 编译 0 warning / 0 error。
- `scripts/smoke-release.ps1 -Version 0.5.1` 通过，20 项检查全部为真（报告：`build/artifacts/tests/smoke/windows-rust-0.5.1-20261103-201024.json`）。

## 0.5.0 — 2026-10-31

### 新增

- **逆回购提醒 Tab**：按真实沪深交易所交易日历提醒，默认 14:50 主提醒、可选 15:20 兜底；不抓取实时行情，不承诺利率或收益。交易日历编译进 EXE，覆盖不足时告警，覆盖结束或日期未知时停止逆回购提醒。
- **信用卡还款提醒 Tab**：本地保存别名、可选银行、每月还款日、提醒时间、提前档位和逾期开关；按自然日计算，29/30/31 日自动钳制到当月最后一天；支持本期已还、撤销和本地去重。
- **通用金融提醒调度**：纯函数计划器 + `finance_shown` 去重台账 + 按 key 的 30 秒重试；逆回购与信用卡使用统一提醒窗口，不伪造 IPO 事件，不进入 IPO 第二通知通道。
- 同步任务改为独立后台线程，金融提醒在任何网络同步之前触发，避免慢网拖过 14:50/15:20 窗口。
- 提醒窗口增加类型、来源、打开标签和主操作参数；健康摘要与设置页通知测试不得覆盖正在显示的正式提醒。
- **「撤销已还」限定在账期窗口内**：只对当前账期或上一账期的已还动作显示撤销。`credit_card_actions` 不裁剪，此前卡片只要还过一次就会永久挂着撤销按钮，几个月后仍能撤掉一条早已离开当前账期的记录，卡片也会长期停在最高的四按钮布局。

### 修复

- **MSI 安装/升级先迁移后备份**：改为先校验并在旧 schema 上创建备份，再执行迁移，保证迁移前备份可用于回滚。
- **同还款日的多张信用卡会互相抑制提醒**：信用卡提醒的 `occurrence_key` 只有「账期:档位:日期」，不含卡片 ID，而调度聚合 `dedup_by` 与已展示台账查询都只按这个键比对。两张同还款日、同提前档位的卡会撞成同一个键，聚合后只剩一条，先展示的那张还会把另一张压在台账里。现在键里带上卡片 ID，并把 `finance_is_shown` 改成按 `(kind, source_id, occurrence_key)` 三元组查询，与表主键一致。
- **单条理财提醒被 IPO 抢占后丢失**：`active_kind` 对单条提醒写的是具体类型（`credit_card` / `reverse_repo`），而抢占恢复分支只认 `"finance"`，于是既不回队列也不释放 in-flight，本次运行不再重试。改为理财窗口统一记 `"finance"`。
- **跨日的逆回购弹窗会把新一天标成已处理**：`今日已处理` 原先写入点击时的日期，周五的弹窗拖到周一才点就会跳过周一所有时点。改为按提醒自身的 `event_date` 写入，日期无法识别或在未来时拒绝执行并提示。
- **健康摘要与通知测试的「打开今日任务」跳错页面**：提醒窗口的打开按钮原先按「非 ipo 就进理财页」判断，而健康摘要（`health`）与通知测试（`test`）的 kind 也不是 `ipo`，点击会打开逆回购页。改为由 Rust 下发显式的 `reminder-opens-finance` 标记。
- **点击理财 Toast 报「任务已不存在」**：理财通知的点击目标是 `finance:repo` / `finance:card:<id>`，通知回调没有识别，全部按 IPO 事件 ID 查详情。现在先分类这两个目标并直接进入对应页面。
- **新卡的还款日与状态不对**：`status_for_card` 在没有已还记录时直接取本月账期，忽略 `first_tracked_cycle`。10/6 新增、每月 5 日还款的卡会显示已经过去的 10/5，首期提前提醒期间还一直显示「未开始」。改为展示账期不早于首个跟踪账期，状态改看「首期账期是否已开始」。
- **后台刷新会覆盖未保存的逆回购设置**：`refresh_finance_ui` 每次刷新都把数据库值回填到开关与时间输入框，而它也会被后台状态刷新调用。现在用 `repo-form-dirty` 标记用户是否改过表单，未保存时不再回填，保存成功后解除标记。
- **逆回购「15:20 兜底」的输入框被顶到整行最右**：`Toggle` 在布局里默认拉伸，把后面的固定宽度输入框挤开，开关和它的时间框隔出一大段空白。给两个时间开关加固定 `width: 174px`（`horizontal-stretch: 0` 对该组件不生效），让「开关 + 输入框」成对相邻。
- **打开理财页面后，排队的下一条提醒不会接着显示**：`on_open_finance` 隐藏提醒窗口后没有调用 `notify_ui()`，而队列只在收到该通知时才会继续处理。A 正在显示、B 已排队时点「打开信用卡 / 打开逆回购」，B 要等其它运行状态变化才可能显示，短时效的逆回购提醒可能因此过期。补上与「稍后处理」「打开详情」「确认已申购」「理财动作」四个出口一致的唤醒。
- **多条理财提醒合并显示时点「打开」进了逆回购页**：多条批次的 `reminder-kind` 被写成 `"finance"`，而页面路由只认 `credit_card` / `reverse_repo`，于是一批全是信用卡的提醒也会跳到逆回购页。现在统一沿用第一条提醒的类型，按钮文案也改为该类型对应的「打开信用卡页 / 打开逆回购页」。
- **信用卡列表「删除」按钮被卡片右边界截断**：动作区原本是「编辑 + 删除」两列并排，需要 2 × `ActionButton` 最小宽度（88/104）再加间距，而列宽只有 116/136；Slint 又会忽略嵌套 Layout 上的 `width`、按子元素 `min-width` 排布，第二个按钮因此被挤到卡片外。改为单列动作区（列宽由按钮最小宽度决定），并按按钮数量精确计算行高（2 个 102、3 个 146、4 个 190），`alignment: start` 让按钮贴顶不再被居中留白拉开；「删除」同时改为两步确认（删除 → 确定删除/取消），避免连带清掉已还记录。
- **表单输入框过窄导致数字被裁**：`LineEdit` 左右各 12px 内边距，聚焦且有内容时右侧还要让出 16px 清除图标，原 56px 的「还款日」输入框只剩 16px 文本区，`55` 被裁并紧贴清除图标。还款日/提醒时间/提前天数分别放宽到 96/108/132px，逆回购的时点输入从 82px 放宽到 104px。
- **逆回购状态卡固定高度会裁掉换行文案**：改为 `min-height`，长错误信息与窄窗下的两行详情不再被裁。
- **改动 `ui/main.slint` 可能不触发重新编译**：`build.rs` 只声明了图标与若干环境变量的重跑条件，缺少 Slint UI 源文件，出现过「源码已改、产物仍是旧界面」的静默不同步；补上 `cargo:rerun-if-changed=ui/main.slint`。
- **顶部健康摘要在默认窗口宽度下被右边缘裁断**：新增两个 Tab 后导航项占约 798px，健康摘要（约 500px）在 1180 宽的默认窗口里放不下，末尾会被窗口直接切掉。改为让摘要占据剩余宽度并用省略号收尾，窄窗给省略号、宽窗完整显示，不再出现半句话。
- **删除卡片会连带清空无关的编辑内容**：原来删除任意一张卡都会重置表单，正在编辑另一张卡时输入会丢。改为只在被删的正是编辑中的那张卡时清空。
- **编辑中的卡被删除后保存会把它「复活」**：保存时若表单 id 在库中已不存在，仍会用同一个 id 落库，等于把刚删掉的卡重新造出来；现在改为明确报错。编辑与删除在读取列表失败时也不再静默无反应，会给出提示。

### 验证

- `rtk cargo test`：199 项通过；`storage::tests::schema_concurrency::concurrent_read_write_paths_do_not_error` 在干净基线 worktree 上同样稳定失败（`attempt to write a readonly database`），属于既有 Windows 测试环境问题，已在完成报告中注明。
- `scripts/build-release.ps1 -PackageMode All` 与 `scripts/validate-build-layout.ps1` 通过，MSI 编译 0 warning / 0 error。
- `scripts/smoke-release.ps1 -Version 0.5.0` 通过，20 项检查全部为真。
- 独立数据目录启动冒烟通过；未写入真实用户数据。

## 0.4.3 — 2026-09-21

### 修复

- **主窗口首次显示后下方出现整条未绘制（透明）区域**：从托盘或二次启动唤出主窗口时会“先按最小尺寸显示、再被程序改成保存的尺寸”，而工作区钳制与居中用到的是 Slint 缓存的旧尺寸，窗口下沿因此落到工作区（屏幕）之外；软件渲染器只提交脏矩形，屏幕外那一段永远画不上，桌面直接透出来，只有再次改尺寸才恢复。现在改为：显示前就下发窗口尺寸（`window_state::apply_initial_main_window_size`，原生窗口创建即目标尺寸）、工作区钳制与居中改用原生客户区几何（`windows_integration::window_client_size`）、原生尺寸与 Slint 缓存对齐后再补一次整窗重绘（`runtime_bridge::schedule_settled_repaint`，轮询间隔 120ms、最多 8 次）。
- 顺带行为变化：没有保存过窗口尺寸时（首次运行或状态文件不可用）按 `ui/main.slint` 的首选尺寸 1180x780 开窗，不再停在最小尺寸 800x500。
- **窗口缩放（DPI）过期检测**：常驻托盘期间显示缩放变化时，Windows 不会把变化通知到隐藏窗口，窗口缓存的 DPI 停留在旧值（实测：显示器 144 DPI、主窗口仍是 96 DPI），界面会一直按旧缩放渲染。现在首次显示时比对 `GetDpiForWindow` 与显示器 DPI，不一致就写一条 WARN（`event=main_window_dpi_stale`），提示重启程序恢复；不在程序内擅自换算尺寸，避免出现窗口变大而界面仍按旧缩放渲染的错配。
- 依赖：`windows` crate 增加 `Win32_UI_HiDpi` 特性（`GetDpiForWindow` / `GetDpiForMonitor`）。

### 变更

- **文档版本基线同步到 0.4.2**：`README.md` 的「当前版本」行、MSI 文件名与便携包文件名此前仍停留在 `0.3.7`，现改为 `0.4.2`，并显式声明版本号以 `Cargo.toml` 为唯一来源。
- **README 补齐 0.4.x 的更新入口行为**：标题行绿色胶囊一次点击即完成下载/校验/退出/安装并以托盘启动新版、版本徽标可点击（悬停高亮）进入设置页手动检查、无可用更新时整块不渲染、显式检查已是最新时提示「当前已是最新版本」、升级完成后一次性回执「已更新到 x.y.z」；回归测试总数由 173 更正为 174。
- **发版清单补上文档同步步骤**（`docs/release-signing-and-updates.md`）：把「修改 `Cargo.toml` 版本」扩展为同时提升 `CHANGELOG.md` 的 `[未发布]`、新增 `RELEASE_NOTES.md` 小节、同步 `README.md` 的版本行与包名，并新增「文档版本一致性自检」一节，明确 `.plan/`、`plan/` 的历史记录按原样保留。
- **历史方案文档登记取代关系**：`.plan/feat/certificate-free-minisign-auto-update.md` 标明被 0.4.x 取代的三处——两步交互改为一次点击、检查节流由 24 小时改为启动 10 分钟地板加周期 6 小时、更新流程取消 Toast 与托盘气泡；`.plan/feat/home-update-banner-and-silent-notifications.md` 与 `.plan/feat/windows-ipo-reminder.md` 更新文首版本基线与状态；`plan/fix.md`、`plan/remediation-plan.md`、`.plan/fix/stock-new-deep-audit.md` 标注为 `0.3.1` 历史基线。
- **代码注释修正**（`src/updater.rs`，仅注释、无行为变化）：`startup_check_due` 的注释误写「最多每小时一次真实网络检查」和「1 小时阈值」，改为与 `STARTUP_CHECK_INTERVAL_MINUTES = 10` 一致。

### 验证

- `rtk cargo fmt` 无残留改动；`rtk cargo test`：175 项通过（新增 1 项「首选窗口尺寸兜底可被接受」用例）。
- 复现与回归（本机 3840x2160 @150%，工作区 3840x2088，用临时数据目录 + 历史保存尺寸驱动托盘/二次启动唤出路径）：
  - 修复前：保存尺寸 1499x1128（逻辑）时窗口外框 2262x1728 落在 y=652，底边 2380 超出屏幕 2160；逐像素比对“窗口显示中”与“应用自身隐藏后”，窗口第 1436～1667 行（232 行）与桌面完全一致，即整条未绘制透明区。
  - 修复后：同一场景窗口完全落在工作区内（底部越界行数 0），整行未绘制区域 0 行；另测「保存尺寸大于工作区（2600x1500）」被正确钳制到 3816x2064 并居中于 (12,12)、「全新数据目录」按 1180x780 打开，均为 0 行未绘制。
  - 日志新增事件 `event=main_window_size_preapplied`（显示前下发尺寸）；`event=main_window_size_restored` 语义不变（尺寸已等于保存值时立即记录，需要补改尺寸时仍在 150ms 后记录），`window-state.json` 读写不受影响，`scripts/smoke-release.ps1` 的既有断言继续有效。
- 未覆盖：多显示器与 100%/150% 缩放切换、任务栏贴靠/还原需要人工走查；`event=main_window_dpi_stale` 只在“显示器缩放变更后窗口仍为旧 DPI”这一本机既有状态下才能触发，本次仅确认检测逻辑与日志输出。
- 发布产物（`rtk cmd /c build.bat --package`）：`build/packages/0.4.3/` 下的 `StockIpoReminder-0.4.3-win-x64.msi`（6,590,464 字节，sha256 `05b4fdd9…f47d3787`）与 `StockIpoReminder-0.4.3-win-x64-portable.zip`（8,162,869 字节，sha256 `f4197078…138d786f9`），以及 `release-manifest.json`、`SHA256SUMS.txt`；`scripts/validate-build-layout.ps1` 通过。
- `scripts/smoke-release.ps1` 全部 20 项断言通过（报告：`build/artifacts/tests/smoke/windows-rust-0.4.3-20260921-092011.json`），其中 `secondLaunchActivatesExistingInstance`、`mainWindowSizePersistence` 覆盖本次改动的窗口路径。
- `scripts/test-signing-update.ps1` 6/6 项通过（报告：`build/artifacts/tests/signing-update/signing-update-0.4.3-20260921-092053.json`）：预哈希签名接受、篡改清单/安装包拒绝、错误密钥拒绝、正式信任根拒绝测试密钥、legacy 签名拒绝。
- 用重建后的发布版 EXE 复验：窗口 2259x1726 落在 (790,181)，底边不越界，整行未绘制区域 0 行；日志为 `event=main_window_size_preapplied` + `event=main_window_size_restored`。
- 更新清单签名（本地、仓库外私钥）：`scripts/sign-update-manifest.ps1 -PasswordFile <vault 密码文件>`，public key id `92c9c7f5823339d5`（与 0.4.2 同一信任根），生成 `update-manifest.json` 与 `update-manifest.json.minisig`，回填 `release-manifest.json` 的 `updateManifest` / `updateManifestSignature` / `updatePublicKeyId`，并重新生成 `SHA256SUMS.txt`；签名脚本内部已用仓库公钥复核，且校验清单版本、安装包名、大小与 SHA-256 与最终 MSI 一致。
- `scripts/audit-release.ps1` 11 项全部通过（报告：`build/artifacts/tests/audit/release-rust-0.4.3-20260921-092616.json`），含 `release.minisign-update-bundle`：更新清单被 minisign 与发布 EXE 各自针对仓库信任根验证通过。
- `scripts/test-update-helper-recovery.ps1` 通过：安装助手对无效 MSI 正确失败（`exit=1620`）并按预期恢复，未触发 UAC。
- 发布资产（8 个，一次性公开）：MSI、便携 ZIP、`README.md`、`RELEASE_NOTES.md`、`release-manifest.json`、`SHA256SUMS.txt`、`update-manifest.json`、`update-manifest.json.minisig`。
- 已发布：GitHub Release `StockIpoReminder v0.4.3`（tag `v0.4.3` → `374108c`，https://github.com/melody0709/StockIpoReminder/releases/tag/v0.4.3），先建 Draft 一次性上传 8 个资产，再从 Draft 下载复核：7/7 资产哈希与 `SHA256SUMS.txt` 一致（该文件本身按设计不含自身）、`update-manifest.json.minisig` 用仓库公钥验签通过（预哈希），随后发布为 stable/latest。
- 发布后只读复核：从公开的 `releases/latest/download/update-manifest.json` 与 `.minisig` 验签通过；用发布 EXE 的 `--update-bundle-self-test` 对公开清单与公开 MSI 跑完客户端验签管线，`success=true, version=0.4.3`（exit 0），说明 0.4.2 及更早版本可正常发现并校验 0.4.3。
- 真实安装闭环（UAC 取消、安装失败、`3010` 与成功路径）仍按文档由人工在独立 `--data-root` 下执行，本次未覆盖。

## 0.4.2 — 2026-09-17

### 变更

- 仅提升版本号，代码与 0.4.1 相同；用于在 0.4.1 上验证「一次点击完成下载、安装并重启到托盘」的完整闭环。
- 发布流程备忘：0.4.1 的首次发布缺少启动即检查的修复，已删除并用重新构建、重新签名的产物替换（旧 0.4.1 从未被安装），再发布 0.4.2 作为升级目标。
- 测试健壮性：`smoke-release.ps1` 的「窗口尺寸恢复」断言等待窗口由 3 秒放宽到 8 秒。紧跟 release 构建后立即跑 smoke 时，该断言曾因机器负载误报一次。

## 0.4.1 — 2026-09-17

### 修复

- **启动检查不再被 6 小时节流吞掉**：新增 `STARTUP_CHECK_INTERVAL_MINUTES = 10` 与 `updater::startup_check_due`，启动路径改用 10 分钟地板（正常重启必定检查，崩溃重启循环仍被限制），周期路径继续用 `AUTOMATIC_CHECK_INTERVAL_HOURS = 6`；`UpdateController::auto_check_if_due` 增加 `startup` 参数，`runtime_bridge` 启动路径传 `true`、周期定时器传 `false`。问题背景：0.4.0 上唯一一次自动检查发生在 18:05，而 0.4.1 于 18:13 发布，用户重启多次也因持久化节流不再检查。
- `AppSettings::automatic_updates_enabled` 默认值 `false` → `true`（检查已静默，新装即可发现更新；仍可在设置页关闭）。
- 版本徽标增加悬停高亮（`hover-touch.has-hover`），提示它是可点击的手动检查入口。
- 「后续提醒与启动」卡片高度由 132px（正好等于内容高度）调整为宽窗口 152px / 窄窗口 216px，修掉窄窗口下 4 个开关被裁切的问题；「启用市场」卡片窄窗口 176px → 184px，两张卡片在宽窗口下保持等高。
- 缩短三处过长开关文案（交易时段健康摘要 / 中签与缴款提醒 / 开机自动启动），避免 `Toggle` 内 `overflow: elide` 截断。
- 托盘右键菜单新增「打开发布页 (GitHub Release)」：`updater::release_page_url()` 改为公开，托盘回调通过 `windows_integration::open_external` 打开固定发布页（失败只写脱敏日志）。

## 0.4.0 — 2026-09-17

### 新增 / 变更

- **更新通知全部去掉**：`UpdateController` 不再调用 `tray.notify`，更新流程不再读取 `toast_enabled`；提示只剩标题行绿色胶囊与按需细行。打新提醒、健康摘要、崩溃报告通知不变。
- **标题行一键更新**：新增 `UpdatePill` 组件（绿色胶囊，紧贴版本徽标右侧）；一次点击 = 下载 + 验证 + 退出 + 安装，helper 以 `--background` 托盘启动新版，删除第二次「重启并更新」确认。`ActionButton` 语义不变，蓝色/红色仍归操作与危险类。
- **无更新时不渲染任何更新元素**：`update-pill-visible` / `update-note` 为空即整块隐藏；版本徽标改为可点击（`open-update-settings`）作为手动检查入口。
- **控制器重构**（`src/ui/background_operations.rs`）：状态收敛为 idle/available/downloading/installing/failed，新增 `DownloadTrigger::{OneClick, Prefetch}`、`request_update`、`dismiss`、`open_manual_download`、`show_startup_receipt`，以及 `publish_*` / `flash_note` 统一下发按钮与细行文案。
- **协议层**（`src/updater.rs`）：新增成功结果 6 小时进程内缓存（`check_for_update_with_cache`，手动检查 `force`）、自动检查节流 24h → 6h、`banner_dismissed_version` 每版本隐藏状态、`probe_latest_release_tag` 只读 302 兜底、`consume_upgrade_receipt` 一次性升级回执；安装结果新增 `version` 与 `consumed` 字段；移除已被替代的 `check_for_update` / `should_notify_version` / `mark_version_notified` 与 `lastNotifiedVersion` 字段。
- **托盘**：新增 `set_update_badge` 与动态菜单项「发现新版本 x.y.z，点击查看」（被动入口，不弹气泡）。
- **UI 文案**：设置页开关改为「自动检查稳定版更新（启动时及每 6 小时）」「发现更新后提前下载（默认关闭）」；更新卡片按钮合并为单个绿色「更新到 x.y.z」。
- **门禁**：`smoke-release.ps1` 的更新 UI 断言改为一次性点击契约，并新增「更新流程不得调用 Toast/气泡」「必须实现缓存、每版本隐藏与只读兜底」三条断言。

### 验证

- `rtk cargo fmt` / `rtk cargo test`：173 项通过（新增缓存节流、每版本隐藏、发布页解析、更高版本判定与升级回执 5 项）。
- `build-release.ps1 -PackageMode All`、`sign-update-manifest.ps1`、`validate-build-layout.ps1`、`smoke-release.ps1`、`test-signing-update.ps1`、`audit-release.ps1` 全部通过（0.4.0 报告时间戳 `20260917-040319`）。

## 0.3.9 — 2026-09-17

### 修复

- 设置页“安全自动更新”卡片在部分窗口下两个开关的文字被垂直裁切：开关行不再强制压缩到固定 34px 高度（行内 ActionButton 最小高 38px 同样占位），卡片高度改为随窗口宽度自适应并预留说明文字换行空间（宽窗口 276px、窄窗口 330px）。
- 本版本同时是 0.3.8 引导版第一次应用内自动更新的目标版本，用于完成 `.plan/feat/certificate-free-minisign-auto-update.md` 第 12 节的真实 MSI 升级闭环。

## 0.3.8 — 2026-09-09

> 状态：已提交（`9da808d`）并发布为 GitHub stable/latest；公开 feed 的清单与签名已复核可用。第 12 节「真实 MSI 升级闭环」以本次 0.3.8 → 0.3.9 的应用内升级为准。

### 变更

- **自动更新信任根迁移**：从「SignPath / CA 代码签名证书 + detached CMS + Authenticode 指纹」改为「内置 Minisign 公钥 + 预哈希签名」，`Cargo.toml` 升至 `0.3.8`，新增依赖 `minisign-verify = "0.2"`，移除三个不再使用的 `Win32_Security_Cryptography_*` feature。
- **更新清单升级为 schema v2**（`src/updater.rs` 重写）：Minisign 预哈希签名覆盖清单原始字节，拒绝 legacy 非预哈希签名；随后依次校验产品、stable 通道、严格递增版本、RFC 3339 发布时间、最低 Windows Build、安装包文件名/大小/SHA-256；下载时执行响应上限、声明长度和增量哈希三重校验。
- **下载与安装拆分为两步**：下载验证完成后落到受控 `pending` 目录并可跨进程恢复；只有用户点击「重启并更新」才退出升级。安装 helper 只接收数据目录与父进程 PID，自行重验全部材料，并以只读句柄锁定 MSI 直到 `msiexec` 结束。
- **新增 `UpdateController`**（`src/ui/background_operations.rs`）：收编原先分散的 `available_update` / `update_check_busy` / `update_install_busy` 三类共享状态，统一负责检查、下载、安装与每小时周期重查，`main.rs` 与各回调层改为持有 `Arc<UpdateController>`。
- **设置项新增**：`AppSettings::automatic_update_download_enabled`（默认 `false`），与原有「启动时及每日自动检查」解耦；自动检查按 24 小时节流，手动检查不受限，同一版本只主动提醒一次。
- **UI**：更新区域重排为两行——第一行「自动检查开关 + 检查更新 + 下载/重试 + 重启并更新」，第二行「发现更新后自动下载（默认关闭）」；新增 `update-ready` / `update-downloading` / `update-download-failed` / `update-download-progress` 属性，区域高度 148px → 216px。
- **托盘通知路由**：新增 `updater::is_update_activation`，更新类 Toast 点击后直达设置页更新分区，不再误入股票详情。
- **发布流水线**：`build.bat` 与各脚本改用 `pwsh`；`build-release.ps1` 移除 `UpdateFeedUrl` 参数与 `Write-DetachedCmsSignature`，`--sign` 退化为可选的纯 Authenticode 能力；CI 新增下载安装 minisign 0.12 的步骤。
- **CI 作用域说明**：`signed-release.yml` 增加头注释与提示步骤，明确它只做构建、可选 Authenticode 与校验，**不产出 Minisign 清单也不发布 Release**；没有代码签名证书时可跳过该工作流，改用本地 `build.bat --package`。

### 新增

- `assets/update-signing/stock-ipo-update.pub`：受信任的正式公钥（编译进 EXE，仓库外私钥签名）。
- `scripts/generate-update-signing-key.ps1`、`scripts/sign-update-manifest.ps1`：密钥生成与清单签名。
- `scripts/test-update-helper-recovery.ps1`：helper 故障恢复测试。
- `tests/fixtures/minisign/`：Minisign 测试密钥与清单 fixture（仅测试用，不被客户端信任）。
- `.gitattributes`、`docs/release-signing-and-updates.md` 重写（无证书更新边界、密钥保管、发版步骤）。

- **发布说明入口**：`updater::release_notes_url` 把签名清单中的 `releaseNotesUrl` 解析为与更新源同目录的安全 HTTPS 地址；只接受不带协议、主机和路径分隔符的相对文件名，绝对 URL、其他主机和 `../` 穿越一律拒绝。设置页据此显示「发布说明」按钮。
- **更新区域信息补全**：显示当前版本与可用版本，卡片高度 216px → 240px，新增 `update-release-notes-url` 属性与 `open-release-notes` 回调。

### 移除

- `build-release.ps1` 中的 detached CMS 签名与 `--sign` 与更新信任的绑定关系。
- `.plan/feat/zero-cost-github-msi-signpath-updates.md` 记录的 SignPath 路线作废（申请未通过），替代方案为 `.plan/feat/certificate-free-minisign-auto-update.md`。

### 验证

- `rtk cargo fmt` / `rtk cargo test`：169 项通过。
- `scripts/test-update-helper-recovery.ps1`：通过（`msiexec` 对无效包返回 1620 → helper 退出码 2、pending 与清单保留、结果文件写入）。
- `validate-build-layout.ps1`、`smoke-release.ps1`、`test-signing-update.ps1`、`audit-release.ps1`：全部通过（0.3.8 最新一轮报告时间戳 `20260917-030603`）。
- 已创建对象：`StockIpoReminder-0.3.8-win-x64.msi`、便携 ZIP、`update-manifest.json` 与其 `.minisig`、`release-manifest.json`、`SHA256SUMS.txt`、`README.md`、`RELEASE_NOTES.md`；发布清单为 `signed: false`（无 Authenticode，属预期）。
- GitHub Release `v0.3.8` 已发布为 stable/latest：上传上述八项资产 → 以重新下载的副本复核（`SHA256SUMS.txt` 全项 OK、`minisign -V` 通过、EXE `--update-bundle-self-test` 返回 `success`）→ 发布后从公开 `releases/latest/download/` 再取一次清单与签名，字节与本地签名副本一致且验签通过。
- 已知未修：`SHA256SUMS.txt` 使用 CRLF 换行，`sha256sum -c` 需先 `tr -d '\r'`；这是历史格式，本版未变更。

---

## 0.3.7 — 2026-09-02

### 移除

- 移除「任务栏按钮提示」开关、运行时闪烁调用与测试按钮：托盘常驻时无可见效果，旧设置的 `flashTaskbar` / `notificationFlashTestPassed` 被安全忽略。
- 通知测试收敛为置顶提醒窗口、Windows Toast、托盘气泡回退和声音；Toast 或气泡任一通过即满足系统通知测试。

## 0.3.6 — 2026-09-02

### 变更

- 设置页改为「常用 / 通知与测试 / 同步 / 高级与维护」四分区，分区切换不丢未保存修改。
- 「恢复默认」「保存设置」「安全退出」固定在设置页底部；通知开关改为等宽布局，通道测试改为双列（窄窗口降为单列）。
- 首次使用提示增加「去完成测试」入口，直达通知与测试分区。

## 0.3.5 — 2026-09-02

### 新增 / 变更

- 设置页底部新增「恢复默认」按钮：先回填、确认后再写入；会清除已完成的通知测试状态。
- 默认「当日未确认任务核验间隔」10 → 20 分钟；第二通知通道的操作控件改为分行排列，避免窄屏重叠。

## 0.3.4 — 2026-09-01

### 新增

- 主窗口尺寸记忆：拖拽后防抖保存（逻辑像素），托盘隐藏 / 关窗 / 安全退出时再同步；最大化与全屏不覆盖普通尺寸，损坏的 `window-state.json` 回退默认。

## 0.3.3 — 2026-09-01

### 变更

- 默认托盘启动：普通启动与开机自启都直接驻留托盘；双击托盘、托盘菜单、通知和重复启动仍显式显示主窗口。
- 主窗口工作区适配与首次重绘延后到用户第一次显式打开；smoke 新增「普通启动保持托盘隐藏」门禁。

## 0.3.2 — 2026-09-01

### 变更

- 提醒降噪：未来任务字段变化静默更新，取消预告、早间、预受理、开盘前和午休结束提醒；打新提醒只落在 09:30–11:30 与 13:00 至安全截止时间内。
- 投递层新增交易时段二次校验，已错过的提醒直接取消；健康摘要仅在确有当日申购任务时进入交易时段提醒。

### 内部

- SQLite 持久化（原 5,500+ 行）按领域拆分；应用入口与 UI、后台运行时按职责拆分，Windows 集成模块分离；行为不变的重构，无新迁移。

## 0.3.1 — 2026-08-27

### 变更

- 宽高紧凑模式解耦：宽度 < 1000、高度 < 650 逻辑像素分别触发各自的压缩策略；最低客户区 760×460 → 800×500。

### 修复 / 内部

- 修复畸形 JSONP 反序括号 panic；`Retry-After` 限 24 小时；有界流式读取（业务 16 MiB / 公告 8 MiB）；HTTPS 重定向逐跳白名单。
- 后台循环改为有界退避（1/5/15/30 秒）而非退出；调度改为单一绝对截止时间调度器，移除每秒 UI 轮询。
- 数据库维护改为每工作日一次；自动备份改为业务指纹触发（保留 7 份 / 512 MiB）；同步无字段变化时不重写表。
- 停止自动下载解析公告 PDF，移除 `lopdf` 与 PDF Worker；东方财富、上交所、北交所查询加时间窗与有界分页。
- 启动提速：数据库准备后台化并加门禁；修复 SQLite 在线备份的等待参数；schema 升至 v10，清除无用的接口正文。
- 回归测试增至 115 项。

## 0.3.0 — 2026-08-26

### 新增

- 加密的第二通知通道：企业微信 / 钉钉 / 飞书机器人与 PushPlus，默认关闭；官方域名与无凭据 HTTPS 白名单、15 秒超时、64 KiB 响应上限。
- Webhook / token 用 Windows DPAPI 绑定当前用户加密，单独存放 `secrets\secondary-notification.dpapi.json`，不进 SQLite、日志与诊断包。
- 独立 Outbox、1/5/15/30 分钟退避（单条最多 5 次）、滚动 1 小时 20 次配额。

### 验证

- SQLite schema v8；新增 7 项定向测试，回归测试增至 74 项；smoke schema v10 增加 DPAPI 与配额门禁。

## 0.2.9 — 2026-08-26

### 新增

- 用户同意的脱敏崩溃报告共享：默认关闭，需同时配置 HTTPS 接收端与隐私政策；上传前二次脱敏（移除命令行、路径、用户名、token 等），24 小时最多 3 次、成功后去重。
- 回归测试增至 67 项；smoke schema v9 增加同意、脱敏、无重定向与限流门禁。

## 0.2.8 — 2026-08-26

### 新增

- Authenticode 与签名发布链：`build.bat --package --sign` 要求 Code Signing EKU 证书、RFC 3161 时间戳与 HTTPS 更新源，签后由 `signtool verify /pa /all` 复核；PFX 密码只从环境变量读取。
- 安全自动更新：detached CMS 清单、证书指纹固定、HTTPS stable 通道、严格递增版本、大小上限与流式 SHA-256、安装前 Authenticode 校验。

## 0.2.7 — 2026-08-26

### 新增

- 安全卸载与数据保留：卸载入口保留用户数据；可选清理要求输入「删除当前用户数据」，MSI 卸载成功后才删除 `%LocalAppData%\StockIpoReminder`。
- 上市日提醒、任务列表性能与筛选、Watchdog 恢复与发布证据。

## 0.2.6 — 2026-08-26

### 变更

- Windows 通知与交互可靠性改进；中签查询与缴款资金提醒；恢复流程与发布验证门禁。

## 0.2.5 — 2026-08-26

### 新增

- 独立非模态置顶提醒窗（不抢焦点）；同一轮按股票聚合到期提醒。
- 自动同步限制在 06:00–22:00 并加定点核验；同步结论改为四态；长退避增加低频健康探测。
- 同 EXE Watchdog（2/10/30 秒退避，10 分钟内最多 3 次）；系统时间诊断纳入 W32Time 状态；新版本首次打开旧库先创建校验备份。

### 验证

- 回归测试 51 项通过。

## 0.2.4 — 2026-08-26

### 变更

- 扩大关键变化判定并引入「已确认但需复核」；同步结论增加必要来源覆盖检查。
- 同步间隔拆分为普通日 / 申购日两档（默认 30 / 10 分钟）；`Retry-After` 解析秒数与 HTTP 日期，上限 24 小时。

### 验证

- 回归测试增至 35 项。

## 0.2.3 — 2026-08-26

### 变更

- 申购确认限制在申购日当天，存储层同步校验，旧版遗留的未来确认会被自动撤销。
- 标题栏版本标识、Per-Monitor V2 DPI 清单与紧凑布局；同步间隔可自定义 5 分钟至 7 天。

### 内部

- 公告源原子写入来源健康；沪市公告增加巨潮镜像；伪 PDF（HTML / WAF / JS 挑战页）落盘前拒绝。
- 开机自启改为 HKCU Run 注册项并清理旧计划任务；发布改为标准 x64 MSI（Program Files、可改目录、Major Upgrade、事务回滚）。
- 生成目录统一收口到 `build/`，新增 `build.bat` 与布局校验。

## 0.2.2 — 2026-08-25

### 变更

- 统一 Cargo、User-Agent、脚本与文档版本为 0.2.2 并重打发布包；发布审计保留 `source.rust-only` 门禁。
- 修复联网同步内存脚本的完成状态判定。

## 0.2.1 — 2026-08-25

### 修复

- 修复 Slint 软件渲染窗口从托盘恢复时只重绘局部、残留桌面背景的问题；所有恢复入口统一整窗重绘。

### 内部

- Rust 实现从 `prototypes/stock-ipo-reminder-rust` 迁移到仓库根目录的标准 Cargo 布局；fixture 迁到 `tests/fixtures`，图标迁到 `assets`。
- 删除旧 C#/.NET/WPF 工程与本地 SDK，正式链路只保留 Rust。

## 0.2.0 — 2026-08-25

### 变更

- 正式运行版本完全迁移到 Rust：Slint 界面 + `windows-rs` 原生集成，四来源同步与健康、诊断、备份全部迁移，单个 EXE 承载 PDF Worker 与安装 / 升级 / 便携 / 审计。

### 验证

- 修复新事件首次同步的外键失败与「撤销确认后状态回写」；修复后台启动与关窗生命周期。
- 16/16 固定 fixture 与存储回归通过；真实联网同步 45 条候选 / 24 个事件 / 7 份公告，四来源成功。

### 已知限制

- 不连接券商、不自动下单；发布物尚未进行 Authenticode 签名。

## 0.1.1 — 2026-08-24

### 变更

- 更换为「市场哨兵」新版图标（16–256 px 多尺寸）；托盘右键菜单显示版本号并固定从底部任务栏上方弹出。

## 0.1.0 — 2026-08-24

首个 Windows 预发布候选版本。

### 新增

- 沪市 / 深市 / 北交所新股申购发现与多源核验；常驻托盘、逐只确认与撤销、关键字段变化重新确认。
- 分级提醒（每小时至每 2 分钟）、午间恢复与安全截止；SQLite 持久化、提醒 Outbox、休眠恢复、来源健康与每日摘要。
- 正式公告下载与哈希校验、字段证据与人工覆盖；数据清理、在线备份、日志轮转、脱敏诊断包与系统时间检查。
- 自包含 `win-x64` 便携 ZIP 与免管理员安装器；发布清单与 SHA-256 校验文件。

### 已知限制

- 不连接券商、不自动下单；首版发布物未进行代码签名；Windows 10 2004+ 的支持声明仍需 smoke 证据。
