<script lang="ts">
  import { tick } from 'svelte';
  import { resolvedLanguage } from './localization';
  import { formatMetricNumber } from './metricFormat';
  import type { ResetCycleUsage } from './types';

  let { cycles }: { cycles: ResetCycleUsage[] } = $props();
  const pageSize = 6;
  const ordered = $derived(
    [...cycles].sort((a, b) => Date.parse(a.startedAt) - Date.parse(b.startedAt)),
  );
  // Offset from the newest page keeps newly received cycles visible by default.
  let pageOffset = $state(0);
  let selectedStart = $state<string | null>(null);
  const pageCount = $derived(Math.max(1, Math.ceil(ordered.length / pageSize)));
  const page = $derived(pageCount - 1 - Math.min(pageOffset, pageCount - 1));
  const visible = $derived(ordered.slice(page * pageSize, (page + 1) * pageSize));
  const selected = $derived(
    visible.find((cycle) => cycle.startedAt === selectedStart) ?? visible.at(-1),
  );
  const quotaMax = $derived(
    Math.max(100, ...visible.map((cycle) => cycle.usage.quotaUsedPercent ?? 0)),
  );
  const tokenMax = $derived(Math.max(1, ...visible.map((cycle) => cycle.usage.tokens)));
  const metrics = ['quota', 'tokens'] as const;
  type Metric = (typeof metrics)[number];

  function label(metric: Metric) {
    return metric === 'quota' ? 'Observed quota consumption (%)' : 'Cycle token usage';
  }

  function value(cycle: ResetCycleUsage, metric: Metric) {
    if (metric === 'tokens')
      return `${formatMetricNumber(cycle.usage.tokens, 'count', 'full')} tokens`;
    return cycle.usage.quotaUsedPercent == null ? 'Unknown' : `${cycle.usage.quotaUsedPercent}%`;
  }

  function fullRange(cycle: ResetCycleUsage) {
    return `${cycle.startedAt} – ${cycle.endedAt ?? 'Current'}`;
  }

  function date(value: string, full = false) {
    return new Intl.DateTimeFormat(
      resolvedLanguage(),
      full
        ? {
            year: 'numeric',
            month: 'short',
            day: 'numeric',
            hour: '2-digit',
            minute: '2-digit',
            second: '2-digit',
            timeZoneName: 'short',
          }
        : { month: 'numeric', day: 'numeric' },
    ).format(new Date(value));
  }

  function height(cycle: ResetCycleUsage, metric: Metric) {
    const amount = metric === 'quota' ? cycle.usage.quotaUsedPercent : cycle.usage.tokens;
    return amount == null
      ? 0
      : (Math.max(0, amount) / (metric === 'quota' ? quotaMax : tokenMax)) * 100;
  }

  function movePage(direction: number) {
    pageOffset = Math.max(0, Math.min(pageCount - 1, pageOffset + direction));
    selectedStart = null;
  }

  function navigate(event: KeyboardEvent, cycle: ResetCycleUsage, metric: Metric) {
    let index = ordered.findIndex((entry) => entry.startedAt === cycle.startedAt);
    if (event.key === 'ArrowLeft') index -= 1;
    else if (event.key === 'ArrowRight') index += 1;
    else if (event.key === 'Home') index = 0;
    else if (event.key === 'End') index = ordered.length - 1;
    else return;
    event.preventDefault();
    index = Math.max(0, Math.min(ordered.length - 1, index));
    selectedStart = ordered[index].startedAt;
    pageOffset = pageCount - 1 - Math.floor(index / pageSize);
    // The event target is only available synchronously; retain the chart across the render.
    const group = (event.currentTarget as HTMLElement).closest('.cycle-charts');
    const start = selectedStart;
    tick().then(() => {
      group
        ?.querySelector<HTMLButtonElement>(`[data-metric="${metric}"][data-start="${start}"]`)
        ?.focus();
    });
  }
</script>

{#if ordered.length}
  <div class="cycle-charts" role="group" aria-label="Reset cycle charts">
    <nav aria-label="Chart cycle range">
      <button
        type="button"
        aria-label="Older cycles"
        disabled={page === 0}
        onclick={() => movePage(1)}>‹</button
      >
      <span>{page * pageSize + 1}–{page * pageSize + visible.length} / {ordered.length}</span>
      <button
        type="button"
        aria-label="Newer cycles"
        disabled={page === pageCount - 1}
        onclick={() => movePage(-1)}>›</button
      >
    </nav>
    {#each metrics as metric (metric)}
      <div class="cycle-chart" role="group" aria-label={label(metric)}>
        <strong>{label(metric)}</strong>
        <div class="chart-plot">
          <div class="chart-scale" aria-hidden="true">
            <span
              >{metric === 'quota'
                ? `${quotaMax}%`
                : formatMetricNumber(tokenMax, 'count', 'row')}</span
            ><span>0</span>
          </div>
          <div class="chart-bars" style={`--columns:${visible.length}`}>
            {#each visible as cycle (cycle.startedAt)}
              {@const unknown = metric === 'quota' && cycle.usage.quotaUsedPercent == null}
              <button
                type="button"
                class:chart-selected={selected?.startedAt === cycle.startedAt}
                class:chart-current={!cycle.endedAt}
                class:chart-unknown={unknown}
                aria-label={`${fullRange(cycle)} · ${label(metric)}: ${value(cycle, metric)}`}
                aria-pressed={selected?.startedAt === cycle.startedAt}
                title={`${fullRange(cycle)} · ${label(metric)}: ${value(cycle, metric)}`}
                data-metric={metric}
                data-start={cycle.startedAt}
                onmouseenter={() => (selectedStart = cycle.startedAt)}
                onfocus={() => (selectedStart = cycle.startedAt)}
                onclick={() => (selectedStart = cycle.startedAt)}
                onkeydown={(event) => navigate(event, cycle, metric)}
                ><span class="chart-bar" style={`height:${height(cycle, metric)}%`}
                ></span>{#if unknown}<span class="chart-missing" aria-hidden="true">?</span
                  >{/if}</button
              >
            {/each}
          </div>
        </div>
      </div>
    {/each}
    <div class="chart-axis" style={`--columns:${visible.length}`} aria-hidden="true">
      {#each visible as cycle (cycle.startedAt)}
        <span class:axis-selected={selected?.startedAt === cycle.startedAt}
          >{date(cycle.startedAt)}{#if !cycle.endedAt}<small>Current</small>{/if}</span
        >
      {/each}
    </div>
    {#if selected}
      <div class="chart-reading" role="status" aria-live="polite" aria-label="Selected cycle">
        <div>
          <span>Started</span><time datetime={selected.startedAt}
            >{date(selected.startedAt, true)}</time
          >
        </div>
        <div>
          <span>Ended</span>{#if selected.endedAt}<time datetime={selected.endedAt}
              >{date(selected.endedAt, true)}</time
            >{:else}<b>Current</b>{/if}
        </div>
        <div><span>Observed quota consumption (%)</span><b>{value(selected, 'quota')}</b></div>
        <div>
          <span>Cycle token usage</span><b
            >{formatMetricNumber(selected.usage.tokens, 'count', 'full')}</b
          >
        </div>
      </div>
    {/if}
    <p class="chart-note">
      Server-observed percentage; tokens cover the whole cycle. Current cycle is in progress.
    </p>
  </div>
{/if}

<style>
  .cycle-charts {
    margin: 8px 0;
    font-size: 10px;
    color: var(--text);
  }
  nav {
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: 8px;
    color: var(--secondary);
    font-variant-numeric: tabular-nums;
  }
  nav button {
    width: 28px;
    height: 26px;
    border: 1px solid var(--separator);
    border-radius: 5px;
    background: var(--background);
    color: var(--text);
    font-size: 18px;
    cursor: pointer;
  }
  nav button:disabled {
    opacity: 0.35;
    cursor: default;
  }
  button:focus-visible {
    outline: 2px solid var(--meter-fill);
    outline-offset: 1px;
  }
  .cycle-chart {
    margin-top: 9px;
  }
  .cycle-chart > strong {
    display: block;
    margin-bottom: 4px;
    font-size: 10px;
    font-weight: 600;
  }
  .chart-plot {
    display: grid;
    grid-template-columns: 35px minmax(0, 1fr);
    height: 68px;
  }
  .chart-scale {
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    padding-right: 4px;
    color: var(--secondary);
    font-size: 9px;
    text-align: right;
  }
  .chart-bars {
    display: grid;
    grid-template-columns: repeat(var(--columns), minmax(0, 1fr));
    gap: 5px;
    border-bottom: 1px solid var(--separator);
    background: linear-gradient(var(--separator), var(--separator)) center / 100% 1px no-repeat;
  }
  .chart-bars button {
    position: relative;
    display: flex;
    align-items: flex-end;
    justify-content: center;
    min-width: 0;
    height: 100%;
    padding: 0 3px;
    border: 0;
    border-radius: 3px 3px 0 0;
    background: transparent;
    cursor: pointer;
  }
  .chart-bars button:hover,
  .chart-bars .chart-selected {
    background: color-mix(in srgb, var(--meter-fill) 13%, transparent);
  }
  .chart-bar {
    display: block;
    width: 100%;
    min-height: 2px;
    max-height: 100%;
    border-radius: 3px 3px 0 0;
    background: color-mix(in srgb, var(--meter-fill) 65%, var(--tray));
  }
  .chart-selected .chart-bar {
    background: var(--meter-fill);
  }
  .chart-current .chart-bar {
    outline: 1px dashed var(--text);
    outline-offset: -1px;
  }
  .chart-unknown .chart-bar {
    min-height: 0;
    background: transparent;
  }
  .chart-missing {
    position: absolute;
    bottom: 6px;
    color: var(--secondary);
    font-size: 14px;
  }
  .chart-axis {
    margin: 4px 0 8px 35px;
    display: grid;
    grid-template-columns: repeat(var(--columns), minmax(0, 1fr));
    gap: 5px;
    color: var(--secondary);
    text-align: center;
    font-size: 9px;
  }
  .chart-axis span {
    min-width: 0;
  }
  .chart-axis small {
    display: block;
    font-size: 8px;
    overflow-wrap: anywhere;
  }
  .axis-selected {
    color: var(--text);
    font-weight: 650;
  }
  .chart-reading {
    padding: 7px;
    border: 1px solid var(--separator);
    border-radius: 6px;
    font-variant-numeric: tabular-nums;
  }
  .chart-reading > div {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 8px;
    margin: 2px 0;
  }
  .chart-reading span {
    color: var(--secondary);
  }
  .chart-reading time {
    text-align: right;
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .chart-reading b {
    font-weight: 600;
    text-align: right;
  }
  .chart-note {
    margin: 6px 0 0;
    color: var(--secondary);
    line-height: 1.4;
  }
</style>
