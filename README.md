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
- **API USD** 与 **Codex credits** 使用独立估算和缓存，不能视为同一账单。
- 周额度金额根据可用本地用量和服务端额度占比估算。缺失日志、未知模型、未记录的云端活动影响完整性；估算不是官方余额或实际扣费。
- 保留其他提供商、托盘／浮动窗口、主题、用量历史和设置功能。详见 [Codex 提供商文档](docs/providers/codex.md)。

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
- **API USD** and **Codex credits** have separate estimates and caches; they are not interchangeable bills.
- Weekly monetary capacity is estimated from available local usage and the server quota percentage. Missing logs, unknown models and unrecorded cloud activity affect completeness. Estimates are not official balances or actual charges.
- Retains other providers, tray/floating modes, themes, history and settings. See the [Codex provider guide](docs/providers/codex.md).

### Development and validation

Requires Node.js 22+, pnpm 11.11.0, stable Rust and [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/). Windows builds use the Rust GNU target and MinGW. Use the clone, development, check and installer commands above.

### Upstream and release boundaries

0.5.3 fixes Windows test builds: application and test executables share a Common Controls 6 manifest, resolving the library-test startup error `0xc0000139` caused by an unavailable `TaskDialogIndirect`. Test executables no longer need manual patching. OMP and Codex usage logic is unchanged.

On 2026-09-22, OpenQuota `main` was [`0b21b35`](https://github.com/deviffyy/OpenQuota/commit/0b21b354e1a0), already in this source baseline. Recent dependency updates, multi-account support and CLI changes are unmerged PRs, not imported as stable fixes. See the [upstream review](docs/upstream-review.md).

Update manually through Releases. A signed in-app feed is unconfigured; checks return `not_configured`. Windows binaries are not Authenticode-signed. See each release for its actual validation scope and [release documentation](docs/releasing.md).

[MIT](LICENSE), retaining the OpenQuota copyright. This is an independent fork, not an official OpenQuota, OpenAI or Oh My Pi product. [Issues and pull requests](https://github.com/vavilonska/tokenledger-omp) are welcome.
