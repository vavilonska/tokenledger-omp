# Codex

TokenLedger OMP tracks Codex subscription limits and usage recorded by the Codex CLI.

## What it tracks

| Metric                           | Meaning                                                      |
| -------------------------------- | ------------------------------------------------------------ |
| Session                          | Usage remaining in the current session window                |
| Weekly                           | Usage remaining in the weekly window                         |
| Spark / Spark Weekly             | Model-specific limits when they are reported for the account |
| Extra Usage                      | Additional usage credits reported by Codex                   |
| Rate Limit Resets                | Available reset credits                                      |
| 5h / Weekly API Value            | Current-cycle spend and inferred allowance at API list price |
| Weekly cycle history             | Actual server-observed windows, including early resets       |
| Today / Yesterday / Last 30 Days | Combined Codex and Oh My Pi local usage            |
| Usage Trend                      | Recent local usage over time                                 |

## Sign-in and local data

Sign in with the Codex CLI by running `codex` and choosing your ChatGPT account. TokenLedger OMP reads the
same authentication data and respects `CODEX_HOME` when it is set. API-key-only sessions can produce
local usage history, but they cannot provide ChatGPT subscription limits.

Spend history is calculated locally from the Codex `sessions` and `archived_sessions` logs. When Oh
My Pi is authenticated to the same Codex account, TokenLedger OMP also reads its session JSONL files
under `~/.omp/agent/sessions/**/*.jsonl`, combines both clients, and prices their tokens at the current
API list rate. No `/stats` command or `stats.db` export is required. Assistant request usage and
`model_usage` entries include real subagent calls; parent `task` aggregate summaries are not added
again. Parsed results are cached by file changes, and inherited records in forked sessions are
deduplicated.

The first scan builds this cache from the historical session files. A large archive can exceed the
provider's refresh timeout; let the local scan finish, then refresh again to use the populated
cache. Later refreshes reuse unchanged files and reparse files that have grown or changed.

Oh My Pi's `~/.omp/agent/agent.db` is still used to match the current OAuth account and obtain weekly
cycle reset anchors, not as the per-request usage source. Historical logs do not provide reliable
per-request account ownership: the existing current-OAuth-account matching restriction still applies,
so matching today does not prove that every historical request belongs to that account.
Missing or deleted session logs cannot be reconstructed. Server subscription quotas are fetched
separately and are shared, so the quota is shown once instead of being added twice.

The cycle allowance estimate divides the combined local API-value spend by the server-reported used
percentage. It changes with model mix and is an estimate, not an OpenAI invoice or a cash balance.
Hover the weekly API-value row to see reset-cycle history. Its boundaries come from the reset times
saved by Oh My Pi, so a banked reset starts a new window instead of being forced into a calendar
week. TokenLedger OMP preserves those reconstructed cycles in its account-scoped database. It does not
guess whether a reset was automatic, global, or banked, and does not upload local records or
credentials.


## API USD and Codex credits

Use the **API USD / Codex credits** switch above the overview, or **Settings → Usage Display →
Cost Display**. The choice is saved and applies to Codex usage rows, model details, reset cycles,
pinned usage metrics and shared screenshots. In credit mode, the overview includes only providers
with a credit estimate; other providers' dollar costs are not converted into Codex credits.

Prices verified on 2026-09-07, per million tokens:

| Model         | Standard API input / cached / output (USD) | Codex input / cached / output (credits) | API Fast | Codex Fast |
| ------------- | ------------------------------------------ | --------------------------------------- | -------- | ---------- |
| GPT-6 Astra   | 10 / 1 / 50                                | 250 / 25 / 1,250                        | 2×       | 2.5×       |
| GPT-5.6 Sol   | 4 / 0.4 / 20                               | 100 / 10 / 500                          | 2×       | 2.5×       |
| GPT-5.6 Terra | 2 / 0.2 / 12                               | 50 / 5 / 300                            | 2×       | 2.5×       |
| GPT-5.6 Luna  | 0.2 / 0.02 / 1.2                           | 5 / 0.5 / 30                            | 2×       | 2.5×       |

API USD uses the currently published Standard API price, excluding Batch/Flex reductions and
third-party or carried client discounts. Sol's published Standard price currently includes OpenAI's
promotion through at least 2026-11-21; no unpublished post-promotion price is invented. API Fast
uses its own multiplier. Astra requests above 272,000 prompt tokens use the published long-context
API rates for the entire request (20 / 2 / 75 USD), before any Fast multiplier.

Codex credit estimates use the independent published token credit card. They do not inherit API
Batch/Flex discounts, API Fast multipliers, API-only long-context surcharges or cache-write charges.
Hover a credit value to see its USD equivalent at the fixed display anchor **2,500 credits = $100**
(1 credit = $0.04). This equivalent is not the API list-price estimate or a subscription invoice.
`cr` is the compact display unit. Unknown/unpublished rates, including Spark research preview,
remain unpriced and partial periods are marked. Missing recorded Fast metadata is estimated at
Standard speed. Estimates cover locally available Codex and same-account Oh My Pi logs, not
unrecorded cloud activity or actual billed credit deductions.

The two histories and their persisted reset-cycle caches are separate. Existing settings default
to API USD, and older snapshots without a credit history are never relabeled as credits. The new
version rebuilds cycle estimates from available logs; it preserves older cached cycle records under
their existing keys. Model details retain sub-cent precision until display formatting.

Sources: [API pricing](https://developers.openai.com/api/docs/pricing),
[Astra](https://developers.openai.com/api/docs/models/gpt-6-astra),
[Codex credit card](https://learn.chatgpt.com/docs/pricing#token-rates),
[Codex Fast](https://learn.chatgpt.com/docs/agent-configuration/speed).

## Troubleshooting

- **Not logged in** — run `codex`, sign in with ChatGPT, then refresh TokenLedger OMP.
- **Subscription usage unavailable** — replace an API-key-only login with a ChatGPT login.
- **Session expired or revoked** — sign in again with `codex`.
- **No local history** — check the active Codex data directory and the value of `CODEX_HOME`.
