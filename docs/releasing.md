# 发布 / Releasing TokenLedger OMP

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
