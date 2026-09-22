# OpenQuota 上游核对 / Upstream review

核对日期 / Reviewed: **2026-09-22**.

- Upstream: https://github.com/deviffyy/OpenQuota
- main: `0b21b354e1a0`, frontend dependency update #55, 2026-08-28.
- Latest published release: `v0.5.0`, 2026-08-24.

## 中文

通过 GitHub API 核对 main、Releases、公开分支和最近更新的开放 PR。Git 祖先关系确认 main 已包含于当前基线，没有新合并修复待移植。此前的托盘恢复、刷新超时隔离、Codex 模型标签和定价补充加载修复均已在基线历史内。

#67 / #64 为依赖升级，#63 为 updater 升级，#47 涉及 WebView2；#56 多账号、#58 CLI、#18 多语言仍是开放 PR。没有将它们作为稳定修复引入，以免改变已有账号合并、窗口和周额度行为。

本次建立独立公开版、移除私有适配器，保留 OMP 和周期估算逻辑；不声称合入不存在的新上游修复。

## English

Checked main, releases, public branches and recently updated open PRs through the GitHub API. Git ancestry confirms main is already included in the baseline; there are no newer merged fixes to transplant. Earlier tray recovery, refresh timeout isolation, Codex model labels and pricing-supplement fixes are already in that history.

#67 / #64 are dependency updates, #63 updates the updater and #47 changes WebView2. Multi-account #56, CLI #58 and multilingual #18 remain open PRs. They were not imported as stable fixes, avoiding unrequested changes to account merging, windows and weekly estimates.

This change establishes an independent public edition, removes the private adapter and preserves OMP/cycle-estimate logic. It does not claim any nonexistent newly merged upstream fix.
