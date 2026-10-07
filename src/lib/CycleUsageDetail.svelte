<script lang="ts">
  import { costReading, costTooltip } from './costDisplay';
  import { formatMetricValue } from './metricFormat';
  import { resolvedLanguage } from './localization';
  import type { ResetCycleUsage } from './types';

  interface Props {
    title: string;
    credits?: boolean;
    cycles: ResetCycleUsage[];
    top: number;
    onEnter: () => void;
    onLeave: () => void;
  }

  let { title, cycles, top, onEnter, onLeave, credits = false }: Props = $props();

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
    const totals: Record<string, number> = {};
    for (const model of cycle.usage.modelBreakdown?.models ?? []) {
      for (const variant of model.variants ?? []) {
        const source = clients.find(
          (client) =>
            variant.model === client ||
            (model.model === 'Other' && variant.model.endsWith(` · ${client}`)),
        );
        if (!source) continue;
        totals[source] = (totals[source] ?? 0) + (variant.costUsd ?? 0);
      }
    }
    return Object.entries(totals).sort((left, right) => right[1] - left[1]);
  }
</script>

<div
  class="cycle-usage-detail"
  style={`top:${top}px`}
  role="tooltip"
  aria-label={`${title} reset cycle history`}
  onmouseenter={onEnter}
  onmouseleave={onLeave}
>
  <h3>Actual reset cycles</h3>
  <div class="cycle-usage-list">
    {#each cycles as cycle (cycle.startedAt)}
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
              : costReading(cycle.usage.estimatedCostUsd, credits)} used</span
          >
          <span title={costTooltip(cycle.usage.estimatedLimitUsd, credits)}
            >{cycle.usage.estimatedLimitUsd == null
              ? 'quota unavailable'
              : `~${costReading(cycle.usage.estimatedLimitUsd, credits)} quota`}</span
          >
        </div>
        <div class="cycle-meta">
          <span>{cycle.usage.quotaUsedPercent?.toFixed(0) ?? '—'}% observed</span>
          <span>{formatMetricValue(cycle.usage.tokens, 'count', 'row', 'tokens')}</span>
        </div>
        {#if sources.length}
          <div class="cycle-sources">
            {#each sources as source (source[0])}
              <span title={costTooltip(source[1], credits)}
                >{source[0]} {costReading(source[1], credits)}</span
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
      width: 304px;
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
