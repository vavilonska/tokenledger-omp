# 发布 / Releasing TokenLedger OMP

## 0.5.7 Cache rate 与用量／凭据修复（本地集成验收完成，待发布）

- 今天／昨天／最近 30 天的用量悬浮明细增加周期汇总、逐模型及已有 variant／来源分行的 Cache rate。
  使用 `Σ缓存读取 tokens / Σ全部 prompt tokens`，跨日和 Other 先累加桶再计算，不平均百分比，也不纳入输出 Token。
- Codex 的 input 已含 cached；OMP 分母为 input + cacheRead + cacheWrite，cacheWrite 已含 1 小时桶，
  input／cacheRead 已含 orchestration，不再额外累加。已有公开 provider 沿各自真实 token 桶贯通；
  Cursor／Grok 保持原先缺价事件不纳入 Token 总数的规则，缓存也不额外纳入这些事件。
- 缓存完整性与价格／速度完整性独立。缺桶、旧留存缺证据或零分母显示未知；已知零读取且有输入显示 0%。
  有源日志通过 parser schema 刷新补充证据；源已删除的旧事件不能从默认零恢复真实缓存率。
- DTO 增加可空 `cacheReadTokens`／`promptTokens`；不改变金额、Token、请求、账号隔离及私有适配器边界。
  中英文沿既有本地化机制，浮层保持窄窗限宽、滚动及悬浮交互。

Cursor／Grok 凭据安全优先：仅 Cursor 本地 SQLite 凭据自动续期，条件写回保护刷新期间的账号切换和退出。
Cursor 系统钥匙串凭据与 Grok CLI JSON 停止主动刷新和写回，由原客户端维护；仍有效的 Token 继续查询，
过期或被拒绝时提示在原客户端重新登录。系统钥匙串来源测试使用构造输入，不代表真实系统钥匙串验收。

Codex 子会话区分会话创建时间与日志写入时间，修复元数据晚写导致可归属子调用漏计的问题。
继承父调用的截止边界使用明确的子会话复制结束／下一任务证据，以及可验证的同秒任务身份；
无法区分的整秒平局不猜测。已有源日志经 parser 刷新重扫，保留去重、周期统计及原账号隔离边界。

旧日志缓存迁移改为事务化恢复：无论旧表仍在、已重命名或已部分复制，重开时补齐未迁移记录，
保留已刷新的目标行；迁移失败回滚，下次打开仍可恢复。旧缓存的时间指纹置零以触发有源重解析，
源已删除的解析历史继续留存。此修复不猜测首次采集前的账号归属，也不声称可从空缓存恢复未观察历史。

2026-10-21 已将四处应用版本同步至 0.5.7，并保留依赖 PR #23／#24／#25。
在隔离用户目录运行 `pnpm install --frozen-lockfile` 后，版本与四项契约检查、
ESLint／Prettier、Svelte（248 文件，0 错误／警告）、279 项前端测试、前端生产构建、
Rust fmt、Clippy `--locked --all-targets -D warnings` 与 Rust `--locked --all-targets`
644 项测试均通过。该轮没有检查失败，但早于随后报告的四项缺陷修复，
不能据此宣称那些缺陷不存在或后续修改已验收。

四项修复稳定后，Rust fmt 与 Clippy `--locked --all-targets -D warnings` 通过；
受影响模块定向补验共 123 项：Codex local usage 69（含 10 项子会话边界回归）、
Cursor 26、Grok 26、旧缓存迁移 2，全部通过。Cursor 使用真实 loopback HTTP 挂起测试
账号切换／退出等五状态；Grok 共享 JSON 只读覆盖六状态，迁移覆盖七持久阶段及失败回滚。
本轮首次 Clippy 因新增 fixture 漏传 `now` 和内部枚举重复前缀失败；补齐参数、
仅重命名私有枚举变体后补验通过，未改变回放逻辑或放宽断言，不宣称首次全绿。

后续已修复长模型明细的可视高度：从实际 top 扣除底部 8px，避免新增缓存行及 Other
variants 将底部挤出窗口。真实 Chromium 组件烟测覆盖 12 组中英／明暗的三周期加权值、
24 组已知零／缺桶／旧字段缺失／零 prompt，以及 4 组 320×520 的长 Other 滚动；
最末 variant 缓存行可见，鼠标移入／移出保持原行为，未改变 Escape 生命周期。

零费用修复后，58 项受影响前端测试、Svelte（249 文件，0 错误／警告）及修改文件
ESLint 通过。另用真实 TotalSpend 组件和生产 Canvas 分享导出验证 5 组：
`$10/1M + $0/1M = $5/MTok`，免费服务图例保留但不绘制虚构弧，全零不当缺失，
partial 零值仍标 partial。首个浏览器 runner 在已完成 28 组边界检查后因本地化分享
按钮选择器失败；修正 runner 后仅补验分享阶段，不宣称该 runner 首次全绿。

独立生产 `scan_into → parser → SQLite → daily DTO` 烟测随后完成：使用合成匹配账号及本地日志，
覆盖不同 prompt 权重的两个模型、大量 output、cacheWrite／1 小时桶和 orchestration 只计一次，
以及零缓存、缺桶与 44 事件 Other 场景；四组均在删源、关闭数据库并重开后保持 DTO 一致。
真实导出 DTO 再由生产 UsageMetric／ModelUsageDetail 在 Chromium 消费，10 组关键 hover 通过：
三周期加权值为 79.5%／9%／45.5%，缺桶汇总未知而已知 Other 仍为 47.4%，零缓存为 0%。
生产价格／速度为 partial 时，已知缓存数据仍展示。初始临时 scanner root 装配遇到 include 宏
限制及 reader 截断，修正临时装配后执行通过；未修改生产语义或禁用 lint。

上述前端矩阵及 scanner 证明生产模块和组件路径，不代表原生 App 或默认账号发现。
新依赖已实际安装、编译并经前端／Rust 消费者检查；通知响应分类、updater 禁止重试等测试通过，
但原生通知送达／点击、全局快捷键、真实二实例唤醒、WebView2 隐藏／恢复、实际系统钥匙串、
真实签名更新、账号网络／OAuth 及安装均未验收。
临时 example、页面、runner、driver、隔离 profile／账本和浏览器／服务均已退出并清理；
去敏 DTO、截图、成功和失败日志留在 `.local/cache-rate-20261021/`，不提交或公开上传。
安装包构建与静态身份核验见下节；尚未发布或安装。历史 0.5.6 验收记录不作为 0.5.7 证据。

English: Unreleased 0.5.7 adds weighted cache-read/all-prompt rates to the three daily-range popups,
including model/source rows and Other folding. Unknown evidence stays unknown, known zero stays 0%,
and pricing/speed completeness is independent. Existing provider token inclusion and public privacy
boundaries remain unchanged. The isolated pre-fix baseline passed frontend checks/build and 644 Rust tests.
After the documented integration repairs, all-target Clippy and 123 affected Rust tests passed.
Production scanner/SQLite retention and ten real-DTO browser cases passed, alongside component interactions
and zero-cost share Canvas exports. Temporary consumers were cleaned. Native-host, real-account, keychain,
signed-update and installation paths remain unverified. This is not a release announcement.

### 0.5.7 Windows 安装包（已构建，未发布／未安装）

双方共同门禁通过后，获授权执行 `corepack pnpm build:installer`，`CARGO_BUILD_JOBS=4`。
命令退出 0；Vite 转换 217 模块，Rust GNU x64 优化构建及 NSIS 成功完成，未重跑已通过测试。
交付文件为 `release/TokenLedger-OMP_0.5.7_x64-setup.exe` 及相邻 `.sha256`，保留旧版本安装包。

- 大小：**10,450,026 bytes**。
- SHA-256：`922c1d03dc7e28edb2a1209972c8494d8b744945c8440990602d096a923151a8`。
- 安装程序、原应用及静态提取 payload 的产品名均为 **TokenLedger OMP**，
  产品／文件版本均为 **0.5.7**；应用 payload 的 PE machine 为 `0x8664`（x64）。
  NSIS 启动 stub 为 `0x014c`，不将 stub 架构误作应用 payload 架构。
- 三者 Authenticode 状态均为 `NotSigned`。没有签名更新或安装验收声明。
- 静态提取安装包后逐字节核对 payload：只允许 Tauri 既有单个
  `__TAURI_BUNDLE_TYPE_VAR_UNK` → `__TAURI_BUNDLE_TYPE_VAR_NSS` 替换，位置为 27,105,464。
  替换后的预期 payload 与提取内容完全一致，SHA-256 均为
  `f47784ad4b48be8977faeff7576d5d8a0634d9bef1f54207531d60c57eaa4f5b`；
  未修改安装包或原应用，也未忽略其他字节差异。

构建日志与身份收据留在 `.local/cache-rate-20261021/installer-build.log` 及
`installer-identity.json`。临时打包／检查 runner、便携解包工具和提取 payload 已清理，
没有运行安装程序或应用，没有新增安装、push、tag 或 GitHub Release。
仅此静态核验不证明原生宿主、通知、快捷键、WebView2、真实账号或系统凭据行为。

English: The 0.5.7 Windows x64 NSIS installer was built and statically inspected, not published or
installed. It is 10,450,026 bytes with the SHA-256 above. Product/file versions are 0.5.7, product name is
TokenLedger OMP, the application payload is x64, and Authenticode is NotSigned. Extracted payload bytes
exactly match the built app after only Tauri's established UNK-to-NSS bundle-marker substitution.
No installer/app execution, installation, remote push/tag/Release or native-host acceptance was performed.

## 0.5.6 低额度刷新、归零周计价与周期图

`package.json`、`src-tauri/Cargo.toml`、`src-tauri/Cargo.lock` 中应用自身包版本及
`src-tauri/tauri.conf.json` 从 0.5.5 同步升至 0.5.6，不升级依赖，不改变应用／数据身份、
价格体系、明确 memory 用途排除规则、签名或 updater 配置。

- 主 Codex 账号周剩余额度大于 0%、不超过 3% 时，刷新间隔从 300 秒缩短为 15 秒。
  归零后恢复普通频率，新周重新跟踪。
- 首次观察周额度归零后，同周期后续 Token 消费按额外额度处理，其费用不计入订阅周期消费
  和额度外推；总消费账本仍保留 Token、请求和原始计价证据。不新增 UI 标记。
- 现有周期历史浮层加入上下两张共享周期横轴的图：上图为服务器观察到的
  `quotaUsedPercent`，下图为周期完整 Token 用量。支持历史范围导航，保留明细、中文和主题；
  未知值不补零。复用现有桌面 UI，不增加独立 WebUI 服务、服务器或依赖。

2026-10-18 本轮公开版复用已完成的共同语义／UI 验收，未重复运行未受影响的整套检查：

- 版本、29 项 Tauri 命令、28 项模型、提供商注册及单实例契约，Rust fmt／Clippy 通过。
  首次 Rust 全套为 638 通过、1 失败；失败是失效的来源显示标签断言。修正测试后，
  首次归零截断用例定向补验通过，保留模型／来源金额及全部 variant tokens 断言；
  bins 编译补验通过。不宣称首次全套全绿。
- 真实生产 scanner、Codex JSONL 与 SQLite 烟测覆盖 API USD／Codex credits：
  cutoff 同时刻消费仍纳入周费用，晚 1ms 后周费用／额度保持 $3.00，总费用／会话费用
  增至 $4.50，周期保留 330,000 tokens／3 请求；删源重开结果一致。
  服务测试另覆盖 0%／3% 边界、15 秒与普通频率、重启恢复、新周及账号隔离。
- Svelte 检查 248 文件、0 错误／警告；首轮定向前端 99 项通过，时钟修复后 App
  定向 60 项通过，8 个受影响文件 ESLint／Prettier 通过。真实 Chromium 中生产组件
  在 320px、明暗主题、中英文覆盖双图、未知／零值、14 周期导航、模型展开和焦点；
  精确 deadline 显示 300s → 5m、15s → 15s，并自然逐秒倒数至 0s。

后端烟测调用真实生产模块，但不是默认账号发现或真实 OAuth／HTTP 验收；前端使用注入 DTO
与隔离事件桥，不代表原生 Tauri 宿主或安装验收。临时 runner、页面和浏览器均已清理，
去敏前端证据留在 `.local/cycle-chart-ui-20261018/`；该目录不提交或公开上传。

### 0.5.6 安装包与公开发布

公开 origin 为 `https://github.com/vavilonska/tokenledger-omp.git`；本轮从公开基线
`89778ef8e823e1e3e913a70047367794c4cbab39` 接续。发布前确认远端 main 未前移、
`v0.5.6` tag／Release 无冲突，禁止移动旧 tag 或覆盖历史资产。

共同门禁闭合后，`corepack pnpm build:installer` 退出 0，Vite 转换 217 模块，
Rust GNU x64 release 构建完成，NSIS 成功生成。构建原名为
`src-tauri/target/x86_64-pc-windows-gnu/release/bundle/nsis/TokenLedger OMP_0.5.6_x64-setup.exe`；
交付副本为 `release/TokenLedger-OMP_0.5.6_x64-setup.exe` 及相邻 `.sha256`。

- 大小：**10,338,797 bytes**。
- SHA-256：`9fa7a2106d784b4409461a5286cd52355a5836ab4c878005d6e31856a678c598`。
- 安装程序和应用资源的产品名称均为 **TokenLedger OMP**，产品／文件版本均为 **0.5.6**；
  应用 PE machine 为 `0x8664`（x64），安装包 Authenticode 状态为 `NotSigned`。
- 交付入口：[v0.5.6 Release](https://github.com/vavilonska/tokenledger-omp/releases/tag/v0.5.6)。
  `v0.5.6` 绑定本轮源码提交，main 正常推送；Release 只上传本包与校验文件。
  `release/` 二进制按既有忽略约定留本地并作为 Release 资产分发，不加入 Git 源码历史。

发布使用环境提供的认证凭据调用 GitHub REST API，不新增 CI／密钥或改变全局工具配置。
发布收据在 `.local/publish-public-056/` 留存远端 main／tag 身份、Release 资产大小，
以及无认证的实际 Release 安装包和校验文件独立下载核对结果；不公开该目录或凭据。
临时下载与发布 runner 在交付时清理。签名与 updater 均未配置，继续手动下载更新。
公开版未在用户宿主安装或启动原生 UI；其他平台安装包、真实 OAuth／HTTP 和 CI 亦未验收。

### English summary

Version 0.5.6 adds 15-second primary-Codex refreshes when weekly remaining quota is above 0% and at
most 3%, returning to ordinary frequency at zero. Consumption after the first observed weekly zero
is excluded from subscription-cycle spend and allowance extrapolation, while the total ledger keeps
tokens, requests and pricing evidence. New weeks restore quota tracking; no UI badge is added.
The existing cycle-history popup adds stacked, shared-axis charts for server-observed quota usage
and complete cycle tokens, with historical navigation, unknown gaps, details, Chinese and themes.
There is no separate WebUI service or new dependency. Existing pricing/memory rules and identities
are unchanged. Backend scanner/SQLite and narrow-window production-component browser acceptance
passed, with targeted repairs and reruns recorded above; neither proves native-host or installation acceptance.

## 0.5.5 memory 估算排除修复

仅明确 `entry_kind == "model_usage"` 且 `purpose == "memory"` 的 OMP 事件排除估算费用贡献；
API USD／Codex credits 及日、会话、周、历史周期均适用。该规则不表示上游调用免费。
保留 tokens、去重请求数、原始成本／速度证据及账号归属；同模型普通请求、title／cache-warm、
其他用途或缺失用途仍按原计价规则，未定价部分继续标记 partial。

成功刷新会按保留的明确用途重算旧周期，包括源已删除但事实仍在账本中的 memory；
保持原周期边界和 recovery baseline，不新增伪 `Unattributed`，不猜测缺失用途。
无需 schema 或周期缓存 key 迁移；版本、应用身份、依赖保持不变。

2026-10-18 在隔离 Windows profile 执行 `corepack pnpm verify`：版本及四项前后端契约、
ESLint／Prettier、Svelte 检查（246 文件，0 错误／警告）、260 项前端测试、前端生产构建、
Rust 格式与 Clippy 通过。首次 Rust 运行 628 通过、3 失败：新删源 fixture 和临时 DTO
导出器漏了 session 头，另一周期 fixture 仍将“未知 title 加已排除 memory”的 subtotal
误断言为 None。补齐合法日志头并改为 Some(0) 且保持 partial 断言后，
`cargo test --manifest-path src-tauri/Cargo.toml --lib --locked providers::codex::local_usage`
定向补验 53 项全部通过；复用未受影响成功证据，不宣称完整套件首次直接通过。

两版各自真实生产 scanner／SQLite 账本／周期代码导出 DTO，短期 Vite 页面挂载实际
`UsageMetric`／`CycleUsageDetail`。每版 12 个 Chromium 交互场景覆盖 API／credits、
当前／删源重开历史、纯 memory／正常同模型 title／额外缺速 title。纯 memory 保留
848 tokens／2 请求、贡献 0 且不推算零额度；正常调用合计 110,848 tokens／3 请求、
$0.30／7.5 credits；额外缺速调用合计 110,958 tokens／4 请求，金额仍 partial。
历史 baseline、token、请求数保持，不产生伪 Unattributed；hover、模型展开和 Escape
关闭通过。此证据证明生产 scanner 加浏览器真实组件，不冒充默认账号发现或原生 Tauri
桌面验收；未修改用户真实数据库、凭据或配置。

两版共同语义／UI 通过后，`corepack pnpm build:installer` 成功生成 GNU x64 NSIS 包。
当前本地交付路径为
`F:/00.My_Pi/TokenLedger-OMP/release/TokenLedger-OMP_0.5.5_x64-setup.exe`，
SHA-256：`f72dd693352e95fc36cc575d37c3d16427edd9e8966d32dbcfbbaf8297fddb2a`，
相邻 `.sha256` 同步写入。版本和产品资源均为 0.5.5／TokenLedger OMP，旧版本安装包保留。
临时 Rust 导出器及页面源码在打包前已清理，Vite 与独立 Chromium 已退出；
去敏 DTO、截图、原始检查日志保留在 `.local/memory-exclusion-20261018/`。
本节构建与浏览器证据不包含安装或原生桌面验收，不沿用下文历史安装记录作为本次证明。

### 0.5.5 发布交付

2026-10-18 核对公开仓库 `origin` 为 `https://github.com/vavilonska/tokenledger-omp.git`，
远端已有版本仅 `v0.5.2`／`v0.5.3`，`v0.5.5` 无 tag／Release 冲突。因此沿用上述
已验收的 0.5.5 包，不升版、不重复构建、不覆盖历史 Release 或资产。
本次交付目标为 [v0.5.5](https://github.com/vavilonska/tokenledger-omp/releases/tag/v0.5.5)，
包含 Windows x64 NSIS 安装包及相邻 `.sha256`；发布验收要求 tag 绑定本轮公开源码提交，
且从 Release 实际下载的安装包 SHA-256 与上述本地包一致。
仅发布公开源码及这两项资产，不上传 `.local` 证据、用户数据或凭据。
继续通过 Releases 手动下载：安装包未做 Authenticode 签名，updater key／endpoints
保持为空；继承 CI 模板仍未启用，未执行的 CI、签名和安装验收不声明通过。

## 0.5.5 请求速度与部分估算修复

`package.json`、`src-tauri/Cargo.toml`、`src-tauri/Cargo.lock` 中应用自身包版本及
`src-tauri/tauri.conf.json` 同步为 0.5.5。本次计价修复不更改依赖、应用身份、签名或更新源；
保留下文已合入的依赖安全维护。

发布前需验证 Standard／Fast／Ultrafast 的 API 与购买积分费率、OMP 主请求与辅助调用的速度
证据、旧缓存／账本在有源和无源场景下的迁移，以及总计、模型、周期、Cost/MTok、分享中的
partial／未知语义。除明确 memory 估算排除项外，未知速度须保留 tokens，不能冒充未知模型、零费用或完整额度。
未采集且已删除的日志无法恢复，旧 Fast 标志也不能恢复 OMP 请求速度；保留首次观察账号归属
和周期边界，不将证据不足的历史金额显示为完整估算。

安装验收还发现旧自启动偏好与系统注册状态可能不一致。关闭一个已经不存在的自启动项现在
按幂等操作处理，避免保存计价显示设置失败；不会为此开启系统自启动。

2026-10-15 验证：版本及前后端契约、ESLint／Prettier、Svelte 类型检查、242 项前端测试、
前端生产构建、Rust 格式与 Clippy 均通过；完整 Rust 套件 625 项通过，无失败或忽略项。
另运行 9 个隔离程序场景，调用本版实际 OMP parser、scanner、SQLite 账本与两套计价函数，
覆盖三速度、两条辅助 memory 请求、删源重开后的重计价及无源旧记录保留 tokens。
本次未执行公开版安装包构建、安装或已安装 EXE 的 GUI 验收，不将源码与隔离运行证据冒充安装验证。

## 0.5.5 纳入的依赖安全维护 / Dependency security maintenance included in 0.5.5

前端锁文件统一使用 `source-map-js` 1.2.2，并将 `postcss-selector-parser` 更新至 7.1.6，
分别修复 [GHSA-68fv-2mgg-jv7q](https://github.com/advisories/GHSA-68fv-2mgg-jv7q) 和
[GHSA-rj75-hqrm-r3gf](https://github.com/advisories/GHSA-rj75-hqrm-r3gf)。
这些是构建／开发工具的传递依赖；此前锁文件维护本身不升级应用版本，也未发布或替换安装包。
本节维护已纳入 0.5.5；本次验证与安装包身份见上文。

The frontend lockfile now uses `source-map-js` 1.2.2 throughout and `postcss-selector-parser` 7.1.6
to address the advisories above. These are transitive build/development dependencies. This lockfile
maintenance itself did not bump the application version or publish/replace installers. These changes
are included in 0.5.5; current validation evidence and installer identity are recorded above.

合并 Rust 依赖更新后，Cargo 锁文件移除已不再被使用的 Windows 0.61 依赖图，并归一化
Windows 0.62 的引用，保证后续 `--locked` 构建无需重写锁文件；此归一化本身不升级 Rust 包。

After merging the Rust dependency updates, the Cargo lockfile removes the unused Windows 0.61
dependency graph and normalizes Windows 0.62 references so `--locked` builds need no lockfile rewrite.
This normalization itself does not upgrade Rust packages.

Claude OAuth 自定义远端端点及凭据请求的重定向必须使用 HTTPS；仅保留本机 loopback HTTP
开发支持，避免 access／refresh token 被发送到明文远端。

Claude OAuth custom remote endpoints and credential-request redirects now require HTTPS. Loopback
HTTP remains available for local development; access/refresh tokens are not sent to remote plaintext endpoints.

Antigravity 本机语言服务不再跟随重定向，防止本机自签证书兼容设置及 CSRF token 扩散到其他端点；
远端客户端继续使用正常证书验证。

Antigravity local language-server requests no longer follow redirects, keeping local self-signed
certificate compatibility and CSRF tokens confined to the discovered loopback service. Remote
requests retain normal certificate validation.

Rustls 更新至 0.23.45，修复 [GHSA-2mjx-qc3c-rqvc](https://github.com/advisories/GHSA-2mjx-qc3c-rqvc)；
同时满足该版本所需的 aws-lc-rs／aws-lc-sys 和 rustls-webpki 依赖约束。

Rustls is updated to 0.23.45 for [GHSA-2mjx-qc3c-rqvc](https://github.com/advisories/GHSA-2mjx-qc3c-rqvc),
along with the aws-lc-rs/aws-lc-sys and rustls-webpki versions required by this release.

Linux GTK 0.18 链仍需要 glib 0.18.5。`src-tauri/vendor/glib` 保留该版本的完整 crates.io 原包和
MIT 许可；原包 SHA-256 为 `233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`。
通过 `[patch.crates-io]` 回移 [gtk-rs/gtk-rs-core#1343](https://github.com/gtk-rs/gtk-rs-core/pull/1343)
（官方合并 `05dff0ee696f9bcd8617cd48c4b812d046d440cb`）的两处指针可变性修复，避免
`VariantStrIter` 将不可变 `&p` 作为 C 函数的输出参数。只更改 `src/variant_iter.rs` 的
`let p` → `let mut p` 和 `&p` → `&mut p`，其余原包源码保持不变，版本仍为 0.18.5。
这是 [GHSA-wrw7-89jp-8q8g](https://github.com/advisories/GHSA-wrw7-89jp-8q8g) 的本地安全回移，
不是伪装为上游 0.20，也不以手动关闭 GitHub 告警代替源代码修复。

The Linux GTK 0.18 graph still requires glib 0.18.5. `src-tauri/vendor/glib` preserves the complete
crates.io package and MIT license; its original archive SHA-256 is recorded above. The crates.io
patch backports the two pointer-mutability changes from upstream PR #1343 (merge commit above);
all other packaged source remains unchanged and the package version stays 0.18.5. This is a local
backport for GHSA-wrw7-89jp-8q8g, not a false upstream 0.20 identity or a manual advisory dismissal.
With GLib development libraries available, run the real optimized iterator regression tests:

```sh
cargo test --manifest-path src-tauri/vendor/glib/Cargo.toml --release variant_iter::tests
cargo tree --manifest-path src-tauri/Cargo.toml --locked --target x86_64-unknown-linux-gnu -i glib
```

2026-10-13 验证：隔离 Linux 环境、GLib 2.88.0、优化构建的 11 项迭代器测试通过；
独立程序通过实际 GVariant FFI 验证正反向迭代、Unicode 和空字符串，Linux GTK/Tauri
依赖树确认使用本地回移。原版在同一工具链下也通过这些测试，因此不宣称已复现优化器误编译。

Validation on 2026-10-13: all 11 optimized iterator tests and a standalone GVariant FFI smoke
passed against GLib 2.88.0, including bidirectional iteration, Unicode and empty strings.
The Linux GTK/Tauri graph resolves to this local backport. The unpatched package also passed
these tests on this toolchain; this does not claim a reproduced optimizer miscompilation.

## 中文

项目在 https://github.com/vavilonska/tokenledger-omp 公开发布。初始公开版本 0.5.2 提供 Windows x64 NSIS 安装包和 SHA-256。

运行 `corepack pnpm build:installer`，产物位于 `src-tauri/target/x86_64-pc-windows-gnu/release/bundle/nsis/`。发布前核对版本、前后端契约、类型检查、相关测试和文件哈希。

原私有仓库、历史、数据和安装程序不进入公开仓库。公开版应用、数据库、凭据与自动启动标识独立，不自动迁移。

签名更新源尚未配置，updater key 和 endpoints 为空。通过 Releases 手动下载，不使用 OpenQuota 的二进制更新源或密钥。本次 Windows 安装包未做 Authenticode 签名。

继承的 CI／签名发布模板保存在 `docs/github-actions/`，没有自动启用。启用前需适配平台、签名和验证流程；未运行的 CI 不宣称通过。

## English

Published at https://github.com/vavilonska/tokenledger-omp. Initial public version 0.5.2 provides a Windows x64 NSIS installer and SHA-256 checksum.

Run `corepack pnpm build:installer`; output is under `src-tauri/target/x86_64-pc-windows-gnu/release/bundle/nsis/`. Check versions, frontend/backend contracts, types, relevant tests and hashes before publishing.

The private repository, history, data and installers are excluded. Application, database, credential and autostart identities are independent; no automatic migration is performed.

Signed updates are unconfigured; updater key/endpoints are empty. Download manually from Releases, never through OpenQuota's binary feed or key. This Windows package is not Authenticode-signed.

Inherited CI/signed-release templates are in `docs/github-actions/`, not enabled workflows. Adapt platform, signing and verification before enabling them; unrun CI is not claimed as passing.

## 0.5.3 修复 / Fix

Windows 清单独立于 Tauri 的图标／版本资源编译，并链接到所有可执行目标，包括 Rust 库单元测试。此前仅正式应用获得 Common Controls 6 声明，库测试会在加载 `TaskDialogIndirect` 时以 `0xc0000139` 退出。现在直接运行 `cargo test --manifest-path src-tauri/Cargo.toml --all-targets --locked`，不需要修改生成的 EXE。

同时修正 Windows 控制台测试的 C# 命名空间，并由 `SystemRoot` 定位系统 PowerShell，避免依赖开发 Shell 的 PATH。OMP 与 Codex 用量、存储和额度估算逻辑未改动。

The Windows manifest is compiled separately from Tauri's icon/version resources and linked into every executable target, including Rust library unit tests. Previously only the application received the Common Controls 6 declaration, so library tests exited with `0xc0000139` while importing `TaskDialogIndirect`. Run `cargo test --manifest-path src-tauri/Cargo.toml --all-targets --locked` directly; generated executables need no manual patching.

The Windows console test also uses a valid C# namespace and resolves system PowerShell through `SystemRoot` instead of relying on a developer shell's PATH. OMP/Codex usage, storage and quota-estimation logic is unchanged.

验证：真实 `cargo test --all-targets --locked` 执行了 564 项 Rust 测试，全部通过，无失败或忽略项；Clippy、版本一致性及格式检查通过。测试 EXE 与发布 EXE 的嵌入清单均包含 Common Controls 6；发布程序通过独立配置下的托盘启动检查，NSIS 安装包重新构建。未新增人工 UI 验收。

Validation: the real `cargo test --all-targets --locked` run passed all 564 Rust tests, with no failures or ignored tests. Clippy, version consistency and formatting checks passed. Both test and release executables contain the Common Controls 6 manifest. The application passed tray startup under a separate profile and the NSIS installer was rebuilt. No new manual UI acceptance was performed.

## 0.5.2 历史验证 / Historical validation

- 前端：223 项用例覆盖通过；Svelte 0 错误／0 警告；ESLint／Prettier 和前后端契约检查通过。
- Rust：Clippy 通过；隔离的生产源码核心测试 113 项通过，涵盖 OMP 账号匹配、去重、周重置锚点、API／credits 定价和周期汇总。
- Windows：NSIS 构建成功；发布 EXE 在独立测试配置中成功初始化托盘和应用。未安装覆盖原私有版，未新增人工 UI 验收。
- 限制：完整桌面 Rust 测试程序编译成功但启动报 `0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND`，未记为通过；隔离核心测试不替代整个桌面套件。

- Frontend: 223 cases covered and passing; Svelte 0 errors/warnings; ESLint/Prettier and frontend/backend contracts passed.
- Rust: Clippy passed; 113 isolated production-source core tests passed, covering OMP account matching, deduplication, weekly anchors, API/credit pricing and cycle aggregation.
- Windows: NSIS built; the release EXE initialized its tray and application under a separate test profile. The private edition was not replaced and no new manual UI acceptance was performed.
- Limitation: the full desktop Rust test executable compiled but could not start (`0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND`); it is not reported as passing. Core tests do not replace the full desktop suite.
