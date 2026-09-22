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

## 0.5.2 验证 / Validation

- 前端：223 项用例覆盖通过；Svelte 0 错误／0 警告；ESLint／Prettier 和前后端契约检查通过。
- Rust：Clippy 通过；隔离的生产源码核心测试 113 项通过，涵盖 OMP 账号匹配、去重、周重置锚点、API／credits 定价和周期汇总。
- Windows：NSIS 构建成功；发布 EXE 在独立测试配置中成功初始化托盘和应用。未安装覆盖原私有版，未新增人工 UI 验收。
- 限制：完整桌面 Rust 测试程序编译成功但启动报 `0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND`，未记为通过；隔离核心测试不替代整个桌面套件。

- Frontend: 223 cases covered and passing; Svelte 0 errors/warnings; ESLint/Prettier and frontend/backend contracts passed.
- Rust: Clippy passed; 113 isolated production-source core tests passed, covering OMP account matching, deduplication, weekly anchors, API/credit pricing and cycle aggregation.
- Windows: NSIS built; the release EXE initialized its tray and application under a separate test profile. The private edition was not replaced and no new manual UI acceptance was performed.
- Limitation: the full desktop Rust test executable compiled but could not start (`0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND`); it is not reported as passing. Core tests do not replace the full desktop suite.
