# TokenLedger OMP

[中文](#中文) · [English](#english)

## 中文

基于 [OpenQuota](https://github.com/deviffyy/OpenQuota) 的独立桌面用量面板。集中查看 AI 编程工具的额度、重置时间、Token 用量和费用估算。公开版保留 **Oh My Pi 与 Codex 同账号用量合并、Codex 周额度估算和重置周期历史**，不包含私有业务适配器。

### 下载与安装

从 [Releases](https://github.com/vavilonska/tokenledger-omp/releases) 下载 Windows x64 安装程序，安装后从托盘打开 **TokenLedger OMP**。本次提供 Windows 安装包；其他平台保留源代码支持，但未发布或验证对应安装包。

- 独立应用标识：`io.github.vavilonska.tokenledgeromp`。
- Windows 数据：`%APPDATA%\io.github.vavilonska.tokenledgeromp`；数据库：`tokenledger-omp.db`。
- 凭据服务：`io.github.vavilonska.tokenledgeromp.api-key`；应用凭据配置路径：`~/.config/tokenledger-omp`。
- 日志：`%LOCALAPPDATA%\TokenLedger OMP\logs\TokenLedger OMP.log`。

不会迁移或共用其他 TokenLedger/OpenQuota 安装的数据、缓存、API Key 存储或自动启动标识。首次启动使用独立设置；仍可读取同机 Codex、OMP 等工具自身的登录和会话记录。高级 `OPENQUOTA_*` 环境变量名称继续兼容。

### OMP 与 Codex

- 读取 OMP 会话 JSONL，通过同一 Codex OAuth 账号校验后合并用量；账号不匹配时跳过。明细保留 **Oh My Pi** 来源。
- 保留 5 小时／周周期用量、服务端观察到的重置时间、历史周周期，以及折叠模型的来源明细。
- 在独立 SQLite 的 `local_usage_events` 薄账本中留存已采集用量；归档、移动、截断或删除对话后不再丢失，重复扫描不重复计数。不保存对话正文或凭据，展示时间窗口保持不变。
- **API USD** 与 **Codex credits** 使用独立估算和缓存，不能视为同一账单。
- 周额度金额根据可用本地用量和服务端额度占比估算。缺失日志、未知模型、未记录的云端活动影响完整性；估算不是官方余额或实际扣费。
- 保留其他提供商、托盘／浮动窗口、主题、用量历史和设置功能。详见 [Codex 提供商文档](docs/providers/codex.md)。

首次更新后请先完成一次完整刷新，再删除希望保留统计的日志。未被采集且没有可恢复解析缓存的历史无法还原；账本保留首次观察时的账号归属，不事后猜测旧日志所属账号。

### 开发与验证

需要 Node.js 22+、pnpm 11.11.0、稳定版 Rust 和 [Tauri 2 开发依赖](https://v2.tauri.app/start/prerequisites/)。Windows 安装包采用 Rust GNU target 与 MinGW 工具链。

```powershell
git clone https://github.com/vavilonska/tokenledger-omp.git
cd tokenledger-omp
corepack pnpm install --frozen-lockfile
corepack pnpm tauri dev
# 检查 / checks
corepack pnpm verify:contracts
corepack pnpm check
corepack pnpm test
cargo test --manifest-path src-tauri/Cargo.toml --lib --locked
# Windows 安装包 / installer
corepack pnpm build:installer
```

### 上游与发布边界

**0.5.6** 在主 Codex 账号周剩余额度大于 0%、不超过 3% 时，将刷新间隔从 300 秒缩短为 15 秒；归零后恢复普通频率，新周重新跟踪。首次观察周额度归零后，同周期后续 Token 消费按额外额度处理，其费用不计入订阅周期消费和额度外推，总消费账本仍保留 Token、请求和费用证据；不新增 UI 标记。既有价格和明确 memory 用途排除规则不变。刷新倒计时按后端精确 deadline 逐秒更新。

本版还在现有周期历史浮层加入上下两张共享周期横轴的图：上图显示服务器观察到的额度消耗百分比，下图显示周期完整 Token 用量；支持历史范围导航，保留明细、中文和主题，未知值不补零。复用现有桌面界面，不增加独立 WebUI 服务、服务器或依赖。

Windows x64 NSIS 包见 [v0.5.6](https://github.com/vavilonska/tokenledger-omp/releases/tag/v0.5.6)：`TokenLedger-OMP_0.5.6_x64-setup.exe`，10,338,797 bytes，SHA-256 为 `9fa7a2106d784b4409461a5286cd52355a5836ab4c878005d6e31856a678c598`，附 `.sha256`。本轮生产 scanner／SQLite、320px 真实浏览器组件和安装包构建通过；公开版未安装或验证原生 UI，完整范围与首次失败／定向修复见[发布记录](https://github.com/vavilonska/tokenledger-omp/blob/main/docs/releasing.md)。

**0.5.4** 更新价格目录并纳入 GPT-6.1 Sol（每百万 tokens：输入 $2、缓存读取 $0.10、缓存写入 $2.50、输出 $10），支持 Fast 与长上下文计价。Codex credits 按购买积分／PAYG 费率独立估算，不代表套餐内额度消耗。新增本地用量薄账本，覆盖 Codex、OMP、Claude、pi、Grok 和 OpenCode 的已采集用量。

0.5.3 修复 Windows 测试构建：正式程序与测试程序共用 Common Controls 6 清单，解决库测试启动时找不到 `TaskDialogIndirect` 的 `0xc0000139` 错误。无需手动修改测试 EXE。OMP 与 Codex 用量逻辑保持不变。

2026-09-22 核对 OpenQuota `main` 为 [`0b21b35`](https://github.com/deviffyy/OpenQuota/commit/0b21b354e1a0)，当前基线已包含它。近期依赖升级、多账号和命令行功能仍是未合并 PR，没有当作稳定修复直接引入。见 [上游核对记录](docs/upstream-review.md)。

目前通过 Releases 手动更新；签名更新源未配置，应用检查更新会返回 `not_configured`。Windows 文件未做 Authenticode 签名。验证范围以 Release 说明为准，见 [发布说明](docs/releasing.md)。

代码采用 [MIT](LICENSE)，保留 OpenQuota 原作者版权。本项目是独立分支，与 OpenQuota、OpenAI 或 Oh My Pi 官方无隶属关系。欢迎 [Issue 和 PR](https://github.com/vavilonska/tokenledger-omp)。

## English

An independent desktop usage dashboard based on [OpenQuota](https://github.com/deviffyy/OpenQuota), showing AI coding quotas, resets, tokens and estimated costs. This public edition preserves **same-account Oh My Pi/Codex usage merging, Codex weekly quota estimates and reset-cycle history**, without private business adapters.

### Download and install

Download the Windows x64 installer from [Releases](https://github.com/vavilonska/tokenledger-omp/releases), then open **TokenLedger OMP** from the tray. This release provides a Windows installer; source support for other platforms remains, but their packages have not been released or validated here.

- Independent application ID: `io.github.vavilonska.tokenledgeromp`.
- Windows data: `%APPDATA%\io.github.vavilonska.tokenledgeromp`; database: `tokenledger-omp.db`.
- Separate credential service: `io.github.vavilonska.tokenledgeromp.api-key`; app credential configuration: `~/.config/tokenledger-omp`.
- Logs: `%LOCALAPPDATA%\TokenLedger OMP\logs\TokenLedger OMP.log`.

It does not migrate or share other TokenLedger/OpenQuota installations' data, caches, API-key storage or autostart identity. It starts with independent settings, while reading the local Codex/OMP clients' own authentication and session records. Advanced `OPENQUOTA_*` environment names remain compatible.

### OMP and Codex

- Reads OMP session JSONL and merges usage only after matching the Codex OAuth account. Mismatched accounts are skipped; breakdowns retain **Oh My Pi** source labels.
- Preserves five-hour/weekly usage, server-observed reset times, historical weekly cycles and source labels inside folded model groups.
- Retains observed usage in the independent SQLite `local_usage_events` ledger. Archiving, moving, truncating or deleting conversations does not erase collected usage, and repeat scans do not double-count it. Conversation text and credentials are not stored; rolling display windows are unchanged.
- **API USD** and **Codex credits** have separate estimates and caches; they are not interchangeable bills.
- Weekly monetary capacity is estimated from available local usage and the server quota percentage. Missing logs, unknown models and unrecorded cloud activity affect completeness. Estimates are not official balances or actual charges.
- Retains other providers, tray/floating modes, themes, history and settings. See the [Codex provider guide](docs/providers/codex.md).

Complete a full refresh after updating before deleting logs you want to retain. Unobserved history without a recoverable parsed cache cannot be restored; retained events keep their first-observed account scope rather than guessing old logs' ownership.

### Development and validation

Requires Node.js 22+, pnpm 11.11.0, stable Rust and [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/). Windows builds use the Rust GNU target and MinGW. Use the clone, development, check and installer commands above.

### Upstream and release boundaries

**0.5.6** shortens the primary Codex account's refresh interval from 300 to 15 seconds while weekly remaining quota is above 0% and at most 3%. Ordinary frequency resumes at zero; a new week restores quota tracking. After the first observed weekly zero, later consumption in that cycle is treated as extra allowance: its cost is excluded from subscription-cycle spend and allowance extrapolation, while the total ledger retains tokens, requests and cost evidence. No new UI badge is added. Existing pricing and explicit memory-purpose exclusions are unchanged. The countdown updates every second from the backend's precise deadline.

The existing cycle-history popup also gains two stacked charts sharing the cycle axis: server-observed quota-used percentage above and complete cycle tokens below. Historical range navigation preserves details, Chinese labels and themes; unknown values remain gaps rather than zero. This uses the existing desktop UI, without a separate WebUI service, server or dependency.

The [v0.5.6 Windows x64 NSIS package](https://github.com/vavilonska/tokenledger-omp/releases/tag/v0.5.6), `TokenLedger-OMP_0.5.6_x64-setup.exe`, is 10,338,797 bytes with SHA-256 `9fa7a2106d784b4409461a5286cd52355a5836ab4c878005d6e31856a678c598` and an adjacent `.sha256`. Production scanner/SQLite, 320px browser-component acceptance and installer build passed. This public edition was not installed or exercised in the native host; see the [release record](https://github.com/vavilonska/tokenledger-omp/blob/main/docs/releasing.md) for scope, original failures and targeted repairs.

**0.5.4** refreshes price catalogs and adds GPT-6.1 Sol (USD per million tokens: input $2, cached input $0.10, cache writes $2.50, output $10), including Fast and long-context pricing. Codex credits independently estimate purchased-credit/PAYG rates, not included plan-limit consumption. A compact local ledger retains observed Codex, OMP, Claude, pi, Grok and OpenCode usage.

0.5.3 fixes Windows test builds: application and test executables share a Common Controls 6 manifest, resolving the library-test startup error `0xc0000139` caused by an unavailable `TaskDialogIndirect`. Test executables no longer need manual patching. OMP and Codex usage logic is unchanged.

On 2026-09-22, OpenQuota `main` was [`0b21b35`](https://github.com/deviffyy/OpenQuota/commit/0b21b354e1a0), already in this source baseline. Recent dependency updates, multi-account support and CLI changes are unmerged PRs, not imported as stable fixes. See the [upstream review](docs/upstream-review.md).

Update manually through Releases. A signed in-app feed is unconfigured; checks return `not_configured`. Windows binaries are not Authenticode-signed. See each release for its actual validation scope and [release documentation](docs/releasing.md).

[MIT](LICENSE), retaining the OpenQuota copyright. This is an independent fork, not an official OpenQuota, OpenAI or Oh My Pi product. [Issues and pull requests](https://github.com/vavilonska/tokenledger-omp) are welcome.
