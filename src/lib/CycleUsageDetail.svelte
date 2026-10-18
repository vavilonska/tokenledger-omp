<script lang="ts">
  import CycleUsageChart from './CycleUsageChart.svelte';
  import { costReading, costTooltip } from './costDisplay';
  import { formatMetricNumber, formatMetricValue } from './metricFormat';
  import { resolvedLanguage } from './localization';
  import type { CycleModelUsage, ResetCycleUsage } from './types';

  interface Props {
    title: string;
    credits?: boolean;
    cycles: ResetCycleUsage[];
    top: number;
    onEnter: () => void;
    onLeave: () => void;
  }

  let { title, cycles, top, onEnter, onLeave, credits = false }: Props = $props();
  const detailId = $props.id();
  let selectedCycle = $state<string | null>(null);
  let pointerInside = false;
  const shares = new Intl.NumberFormat(resolvedLanguage(), {
    style: 'percent',
    maximumFractionDigits: 2,
  });

  function costShareReason(cycle: ResetCycleUsage, model: CycleModelUsage) {
    if (model.costUsd == null) return 'Model cost is unknown.';
    if (model.costComplete !== true) return 'Model cost is partial or unknown.';
    if (!cycle.usage.estimateComplete) return 'Cycle cost is partial or unknown.';
    if (cycle.usage.estimatedCostUsd == null) return 'Cycle cost is unknown.';
    if (cycle.usage.estimatedCostUsd <= 0) return 'Cycle cost must be greater than zero.';
    return undefined;
  }

  function dismissModels(event: KeyboardEvent) {
    if (event.key !== 'Escape') return;
    if (
      event.currentTarget instanceof HTMLElement &&
      event.currentTarget.classList.contains('cycle-model-detail')
    ) {
      event.currentTarget
        .closest('section')
        ?.querySelector<HTMLButtonElement>('.cycle-token-trigger')
        ?.focus();
    }
    selectedCycle = null;
    event.stopPropagation();
  }

  function range(cycle: ResetCycleUsage) {
    const dateTime = new Intl.DateTimeFormat(resolvedLanguage(), {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    });
    const start = dateTime.format(new Date(cycle.startedAt));
    if (!cycle.endedAt) return `${start} – now`;
    return `${start} – ${dateTime.format(new Date(cycle.endedAt))}`;
  }

  function sourceTotals(cycle: ResetCycleUsage) {
    const clients = ['Codex', 'Oh My Pi', 'pi'];
    const totals: Record<string, number | null> = {};
    for (const model of cycle.usage.modelBreakdown?.models ?? []) {
      for (const variant of model.variants ?? []) {
        const source = clients.find(
          (client) =>
            variant.model === client ||
            (model.model === 'Other' && variant.model.endsWith(` · ${client}`)),
        );
        if (!source) continue;
        if (!(source in totals)) totals[source] = null;
        if (variant.costUsd !== null) totals[source] = (totals[source] ?? 0) + variant.costUsd;
      }
    }
    return Object.entries(totals).sort((left, right) => (right[1] ?? -1) - (left[1] ?? -1));
  }
</script>

<div
  class="cycle-usage-detail"
  style={`top:${top}px;max-height:calc(100dvh - ${top + 8}px)`}
  role="tooltip"
  aria-label={`${title} reset cycle history`}
  onmouseenter={() => {
    pointerInside = true;
    onEnter();
  }}
  onmouseleave={(event) => {
    pointerInside = false;
    if (!event.currentTarget.contains(document.activeElement)) onLeave();
  }}
  onfocusin={onEnter}
  onfocusout={(event) => {
    if (!pointerInside && !event.currentTarget.contains(event.relatedTarget as Node | null))
      onLeave();
  }}
>
  <h3>Actual reset cycles</h3>
  <CycleUsageChart {cycles} />
  <div class="cycle-usage-list">
    {#each cycles as cycle, index (cycle.startedAt)}
      {@const sources = sourceTotals(cycle)}
      <section class:cycle-current={!cycle.endedAt}>
        <div class="cycle-heading">
          <strong>{range(cycle)}</strong>
          {#if !cycle.endedAt}<i>Current</i>{/if}
        </div>
        <div class="cycle-values">
          <span title={costTooltip(cycle.usage.estimatedCostUsd, credits)}
            >{cycle.usage.estimatedCostUsd == null
              ? '—'
              : `${costReading(cycle.usage.estimatedCostUsd, credits)}${cycle.usage.estimateComplete ? '' : ' partial'}`}
            used</span
          >
          <span
            title={cycle.usage.estimateComplete
              ? costTooltip(cycle.usage.estimatedLimitUsd, credits)
              : undefined}
            >{!cycle.usage.estimateComplete || cycle.usage.estimatedLimitUsd == null
              ? 'quota unavailable'
              : `~${costReading(cycle.usage.estimatedLimitUsd, credits)} quota`}</span
          >
        </div>
        <div class="cycle-meta">
          <span>{cycle.usage.quotaUsedPercent?.toFixed(0) ?? '—'}% observed</span>
          <button
            type="button"
            class="cycle-token-trigger"
            aria-label="Show cycle model usage"
            aria-expanded={selectedCycle === cycle.startedAt}
            aria-haspopup="dialog"
            aria-controls={`${detailId}-${index}`}
            title={formatMetricValue(cycle.usage.tokens, 'count', 'full', 'tokens')}
            onmouseenter={() => (selectedCycle = cycle.startedAt)}
            onfocus={() => (selectedCycle = cycle.startedAt)}
            onclick={() => (selectedCycle = cycle.startedAt)}
            onkeydown={dismissModels}
            >{formatMetricValue(cycle.usage.tokens, 'count', 'row', 'tokens')}</button
          >
        </div>
        {#if selectedCycle === cycle.startedAt}
          <div
            id={`${detailId}-${index}`}
            class="cycle-model-detail"
            role="dialog"
            aria-modal="false"
            aria-label="Cycle model usage"
            tabindex="0"
            onkeydown={dismissModels}
          >
            {#if cycle.modelUsage == null}
              <p>Model usage is unknown for this cycle.</p>
            {:else if !cycle.modelUsage.length}
              <p>No model usage in this cycle.</p>
            {:else}
              <table>
                <thead>
                  <tr>
                    <th scope="col">Model</th>
                    <th scope="col">Token usage<span>Cycle token share</span></th>
                    <th scope="col">Consumption<span>Cycle cost share</span></th>
                    <th scope="col">Requests</th>
                  </tr>
                </thead>
                <tbody>
                  {#each [...cycle.modelUsage].sort((a, b) => b.totalTokens - a.totalTokens || a.model.localeCompare(b.model)) as model (model.model)}
                    {@const unavailableCostShare = costShareReason(cycle, model)}
                    <tr>
                      <th scope="row">{model.model}</th>
                      <td>
                        {formatMetricNumber(model.totalTokens, 'count', 'full')}
                        <span
                          >{cycle.usage.tokens > 0
                            ? shares.format(model.totalTokens / cycle.usage.tokens)
                            : '—'}</span
                        >
                      </td>
                      <td>
                        <span class="cycle-model-cost" title={costTooltip(model.costUsd, credits)}
                          >{costReading(model.costUsd, credits, true)}
                          {#if model.costUsd != null && model.costComplete !== true}
                            <small>partial</small>
                          {/if}</span
                        >
                        <span title={unavailableCostShare}
                          >{unavailableCostShare === undefined
                            ? shares.format(model.costUsd! / cycle.usage.estimatedCostUsd!)
                            : '—'}</span
                        >
                      </td>
                      <td class:cycle-model-unknown={model.requestCount == null}
                        >{model.requestCount == null
                          ? 'Unknown'
                          : formatMetricNumber(model.requestCount, 'count', 'full')}</td
                      >
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/if}
            <p>
              Token shares use cycle tokens. Cost shares use complete cycle costs in the selected
              pricing basis.
            </p>
          </div>
        {/if}
        {#if sources.length}
          <div class="cycle-sources">
            {#each sources as source (source[0])}
              <span title={costTooltip(source[1], credits)}
                >{source[0]}
                {source[1] === null ? '—' : costReading(source[1], credits)}{source[1] !== null &&
                !cycle.usage.estimateComplete
                  ? ' partial'
                  : ''}</span
              >
            {/each}
          </div>
        {/if}
      </section>
    {/each}
  </div>
  <p>Server-observed windows; reset cause is not inferred.</p>
</div>

<style>
  :global {
    .cycle-usage-detail {
      position: fixed;
      right: 8px;
      z-index: 100;
      box-sizing: border-box;
      width: min(360px, calc(100vw - 16px));
      max-height: calc(100vh - 16px);
      padding: 14px;
      overflow-y: auto;
      border: 1px solid var(--separator);
      border-radius: 12px;
      color: var(--text);
      background: color-mix(in srgb, var(--tray) 97%, transparent);
      box-shadow: 0 12px 36px rgba(0, 0, 0, 0.28);
      animation: cycle-detail-in 120ms ease-out both;
    }

    .cycle-usage-detail h3 {
      margin: 0 0 7px;
      font-size: 13px;
      font-weight: 650;
      line-height: 17px;
    }

    .cycle-usage-list {
      display: grid;
    }

    .cycle-usage-list section {
      display: grid;
      gap: 3px;
      padding: 8px 0;
      border-top: 1px solid var(--separator);
    }

    .cycle-usage-list section:first-child {
      border-top: 0;
    }

    .cycle-heading,
    .cycle-values,
    .cycle-meta {
      display: flex;
      align-items: baseline;
      justify-content: space-between;
      gap: 8px;
      font-size: 10px;
      line-height: 14px;
    }

    .cycle-heading strong {
      font-size: 11px;
      font-weight: 650;
      font-variant-numeric: tabular-nums;
    }

    .cycle-heading i {
      padding: 1px 5px;
      border-radius: 999px;
      color: var(--meter-fill);
      background: color-mix(in srgb, var(--meter-fill) 13%, transparent);
      font-style: normal;
      font-size: 9px;
      line-height: 12px;
    }

    .cycle-values span:first-child {
      font-weight: 650;
    }

    .cycle-values span,
    .cycle-meta span {
      font-variant-numeric: tabular-nums;
      white-space: nowrap;
    }

    .cycle-meta {
      color: var(--secondary);
    }

    .cycle-token-trigger {
      padding: 0;
      border: 0;
      border-radius: 3px;
      color: inherit;
      background: none;
      font: inherit;
      font-variant-numeric: tabular-nums;
      cursor: default;
      white-space: nowrap;
    }

    .cycle-token-trigger:hover,
    .cycle-token-trigger:focus-visible {
      outline: 2px solid var(--button-hover);
      background: var(--button-hover);
    }

    .cycle-model-detail {
      max-height: min(260px, 50dvh);
      margin-top: 5px;
      overflow: auto;
      overscroll-behavior: contain;
      border: 1px solid var(--separator);
      border-radius: 6px;
      font-size: 10px;
      line-height: 14px;
    }

    .cycle-model-detail:focus-visible {
      outline: 2px solid var(--meter-fill);
      outline-offset: 1px;
    }

    .cycle-model-detail table {
      width: 100%;
      table-layout: fixed;
      border-collapse: collapse;
      font-variant-numeric: tabular-nums;
    }

    .cycle-model-detail th,
    .cycle-model-detail td {
      box-sizing: border-box;
      padding: 5px 3px;
      border-bottom: 1px solid var(--separator);
      text-align: right;
      vertical-align: top;
    }

    .cycle-model-detail thead {
      position: sticky;
      top: 0;
      background: var(--tray);
    }

    .cycle-model-detail th:first-child {
      width: 24%;
      overflow-wrap: anywhere;
      text-align: left;
      font-weight: 550;
    }

    .cycle-model-detail th:nth-child(2) {
      width: 30%;
    }

    .cycle-model-detail th:nth-child(3) {
      width: 31%;
    }

    .cycle-model-detail th:last-child {
      width: 15%;
      overflow-wrap: anywhere;
    }

    .cycle-model-detail td {
      white-space: nowrap;
    }

    .cycle-model-detail td.cycle-model-unknown {
      white-space: normal;
      overflow-wrap: anywhere;
    }

    .cycle-model-detail th > span,
    .cycle-model-detail td > span:not(.cycle-model-cost) {
      display: block;
      color: var(--secondary);
      font-size: 9px;
      font-weight: 400;
    }

    .cycle-model-detail small {
      display: block;
      color: var(--secondary);
      font-size: 9px;
    }

    .cycle-model-detail p {
      margin: 6px;
      color: var(--secondary);
      font-size: 9px;
      line-height: 13px;
    }

    .cycle-sources {
      display: flex;
      flex-wrap: wrap;
      gap: 4px 8px;
      color: var(--secondary);
      font-size: 9px;
      line-height: 12px;
    }

    .cycle-usage-detail > p {
      margin: 7px 0 0;
      color: var(--secondary);
      font-size: 9px;
      line-height: 12px;
      text-align: center;
    }

    @keyframes cycle-detail-in {
      from {
        opacity: 0;
        transform: translateY(-2px) scale(0.98);
      }
      to {
        opacity: 1;
        transform: translateY(0) scale(1);
      }
    }
  }
</style>
