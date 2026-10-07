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
| 5h / Weekly API Value            | Matched Codex / Oh My Pi consumption and inferred allowance  |
| Weekly cycle history             | Actual server-observed windows, including early resets       |
| Today / Yesterday / Last 30 Days | Combined Codex and Oh My Pi local usage                      |
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
again. Parsed results are cached by file changes; a separate compact usage ledger preserves observed
requests independently of their transcript files. Inherited fork records and archived copies are deduplicated.
Distinct OMP calls without response or entry IDs retain their occurrence within the session, even when
their timestamps and token counts are identical. Compatible old parsed caches can also seed the ledger
after their source files have been removed.

The first scan builds the cache and ledger from historical session files. A large archive can exceed the
provider's refresh timeout; let the local scan finish, then refresh again to use the populated
cache. Later refreshes reuse unchanged files and reparse files that have grown or changed.

The `local_usage_events` ledger lives in the application's existing `tokenledger-omp.db`. It stores
only usage facts (model, time, token buckets, speed and request identity), not conversation text or
credentials. Cached file rows remain disposable; removing a source file does not remove its ledger
entries. Finish a complete refresh before deleting logs you want to retain.

Oh My Pi's `~/.omp/agent/agent.db` is still used to match the current OAuth account and obtain weekly
cycle reset anchors, not as the per-request usage source. Historical logs do not provide reliable
per-request account ownership: the existing current-OAuth-account matching restriction still applies,
so matching today does not prove that every historical request belongs to that account.
Usage already observed by this application survives archiving, moving, truncation, deletion and
application restarts. Unobserved deleted sessions cannot be reconstructed without a compatible
parsed cache. Retained events stay with the account that first observed them; this does not prove
the historical ownership of old logs. Server subscription quotas are fetched separately and are
shared, so the quota is shown once instead of being added twice.

Cycle estimates use only Codex and same-account Oh My Pi sessions whose local token records can be
paired with server-reported **per-thread** consumption in the same reset cycle. The displayed cycle
spend, tokens and percentage describe that matched sample, not total account activity. The allowance
estimate divides the matched sample's priced tokens by its matching consumed percentage. Account-wide
quota percentages are used to show shared limits, never as the denominator for this estimate.
No additional pi or cloud records are added to these samples; existing daily usage tracking
is unchanged. Unavailable attribution is not zero usage. Sessions whose lifetime usage
cannot be placed in one cycle, incomplete observations and unsupported prices cannot produce a quota
estimate. It changes with model mix and remains an estimate, not an OpenAI invoice or a cash balance.
The server reports per-thread percentages, not matching token totals. If the same thread continues
on another device or in the cloud without updating this device's journal, its percentage may include
unobserved calls. Matching therefore relies on the local journal covering that thread's usage;
detected missing or conflicting records are excluded, but cross-device completeness is not proven.
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

Prices verified against official OpenAI documentation on 2026-09-30, per million tokens:

| Model         | Standard API input / cached / output (USD) | Codex input / cached / output (credits) | API Fast | Purchased credit Fast |
| ------------- | ------------------------------------------ | --------------------------------------- | -------- | --------------------- |
| GPT-6 Astra   | 10 / 1 / 50                                | 250 / 25 / 1,250                        | 2×       | 2×                    |
| GPT-6.1 Sol   | 2 / 0.1 / 10                               | 50 / 2.5 / 250                          | 2×       | 2×                    |
| GPT-6 Sol     | 2 / 0.2 / 10                               | 50 / 5 / 250                            | 2×       | 2×                    |
| GPT-6 Luna    | 0.1 / 0.01 / 0.5                           | 2.5 / 0.25 / 12.5                       | 2×       | 2×                    |
| GPT-5.6 Sol   | 4 / 0.4 / 20                               | 100 / 10 / 500                          | 2×       | 2×                    |
| GPT-5.6 Terra | 2 / 0.2 / 12                               | 50 / 5 / 300                            | 2×       | 2×                    |
| GPT-5.6 Luna  | 0.2 / 0.02 / 1.2                           | 5 / 0.5 / 30                            | 2×       | 2×                    |

API USD uses the currently published Standard API price, excluding Batch/Flex reductions and
third-party or carried client discounts. GPT-5.6 Sol's published Standard price includes OpenAI's
promotion through at least 2026-11-21; no unpublished post-promotion price is invented. API Fast
uses its own multiplier. For GPT-6 and GPT-5.6, requests above 272,000 prompt tokens use the
published long-context API rates for the entire request: 2× input and cache rates, 1.5× output,
before any Fast multiplier. API cache writes cost 1.25× uncached input.

Codex credit estimates use the independent published token credit card. They do not inherit API
Batch/Flex discounts, API Fast multipliers, API-only long-context surcharges or cache-write charges.
Hover a credit value to see its USD equivalent at the fixed display anchor **2,500 credits = $100**
(1 credit = $0.04). This equivalent is not the API list-price estimate or a subscription invoice.
At Standard speed, the seven models in the table have the same input, cached-input and output
numbers after this display conversion. Credit estimates use purchased-credit/Enterprise PAYG
Fast rates (2×), not included subscription-limit consumption (2.5×). These are different quantities;
do not infer subscription usage from the displayed credit estimate.
GPT-5.4 mini is also a small published exception: its output is 113 credits per million versus
the API equivalent of 112.5 credits. At the display conversion, GPT-Image-2 image-token credits
cost 2× its API image-token rates;
image tokens are outside this local text-token estimator. Purchased credit prices and included
subscription allowances depend on the plan and are not inferred from this display conversion.
`cr` is the compact display unit. Unknown/unpublished rates, including Spark research preview,
remain unpriced and partial periods are marked. Missing recorded Fast metadata is estimated at
Standard speed. Estimates cover locally available Codex and same-account Oh My Pi logs, not
unrecorded cloud activity or actual billed credit deductions.

The two histories and their persisted reset-cycle caches are separate. Existing settings default
to API USD, and older snapshots without a credit history are never relabeled as credits. Matched
cycle estimates use new cache keys; old estimates based on account-wide percentages are removed
from displayed snapshots, including when a refresh fails. Existing daily history and the old stored
cycle records are preserved. Model details retain sub-cent precision until display formatting.

Sources: [API pricing](https://developers.openai.com/api/docs/pricing),
[Astra](https://developers.openai.com/api/docs/models/gpt-6-astra),
[Sol 6.1](https://developers.openai.com/api/docs/models/gpt-6.1-sol),
[Sol](https://developers.openai.com/api/docs/models/gpt-6-sol),
[Luna](https://developers.openai.com/api/docs/models/gpt-6-luna),
[Codex credit card](https://learn.chatgpt.com/docs/pricing#token-rates),
[Codex Fast](https://learn.chatgpt.com/docs/agent-configuration/speed).

## Troubleshooting

- **Not logged in** — run `codex`, sign in with ChatGPT, then refresh TokenLedger OMP.
- **Subscription usage unavailable** — replace an API-key-only login with a ChatGPT login.
- **Session expired or revoked** — sign in again with `codex`.
- **No local history** — check the active Codex data directory and the value of `CODEX_HOME`.
