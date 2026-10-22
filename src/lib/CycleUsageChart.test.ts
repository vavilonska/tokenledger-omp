import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import CycleUsageChart from './CycleUsageChart.svelte';
import CycleUsageDetail from './CycleUsageDetail.svelte';
import UsageMetric from './UsageMetric.svelte';
import { installLocalization, setLanguagePreference } from './localization';
import type { ResetCycleUsage } from './types';

function cycle(day: number, percent: number | null = 20, tokens = day * 1234): ResetCycleUsage {
  return {
    startedAt: `2026-09-${String(day).padStart(2, '0')}T09:59:41Z`,
    endedAt: `2026-09-${String(day + 1).padStart(2, '0')}T12:17:03Z`,
    scheduledResetAt: '2026-10-01T00:00:00Z',
    usage: {
      tokens,
      quotaUsedPercent: percent,
      estimatedCostUsd: 3,
      costEstimated: true,
      estimateComplete: true,
    },
  };
}

function quotaChart() {
  return screen.getByRole('group', { name: 'Observed quota consumption (%)' });
}

function tokenChart() {
  return screen.getByRole('group', { name: 'Cycle token usage' });
}

afterEach(() => {
  cleanup();
  setLanguagePreference('english');
  vi.useRealTimers();
});

describe('actual reset cycle charts', () => {
  it.each([undefined, null])(
    'does not render missing observed percentage %s as zero',
    (percent) => {
      const current = cycle(2);
      current.usage.quotaUsedPercent = percent;
      render(CycleUsageChart, { cycles: [current] });
      expect(within(quotaChart()).getByRole('button')).toHaveAccessibleName(/: Unknown$/);
      expect(screen.getByRole('status')).toHaveTextContent('Unknown');
      expect(within(tokenChart()).getByRole('button')).toHaveAccessibleName(/2,468 tokens$/);
    },
  );

  it('aligns chronological cycles without mutating input, distinguishing unknown and zero', async () => {
    const latest = { ...cycle(9, 37.25, 1_234_567), endedAt: null };
    const missing = cycle(2, null, 123);
    const zero = cycle(5, 0, 0);
    const cycles = Object.freeze([latest, missing, zero]) as unknown as ResetCycleUsage[];
    render(CycleUsageChart, { cycles });
    const quotas = within(quotaChart()).getAllByRole('button');
    const tokens = within(tokenChart()).getAllByRole('button');
    expect(quotas.map((button) => button.getAttribute('aria-label'))).toEqual([
      `${missing.startedAt} – ${missing.endedAt} · Observed quota consumption (%): Unknown`,
      `${zero.startedAt} – ${zero.endedAt} · Observed quota consumption (%): 0%`,
      `${latest.startedAt} – Current · Observed quota consumption (%): 37.25%`,
    ]);
    expect(tokens.map((button) => button.getAttribute('aria-label'))).toEqual([
      `${missing.startedAt} – ${missing.endedAt} · Cycle token usage: 123 tokens`,
      `${zero.startedAt} – ${zero.endedAt} · Cycle token usage: 0 tokens`,
      `${latest.startedAt} – Current · Cycle token usage: 1,234,567 tokens`,
    ]);
    expect(tokens[2]).toHaveAttribute(
      'title',
      `${latest.startedAt} – Current · Cycle token usage: 1,234,567 tokens`,
    );
    expect(screen.getByRole('status', { name: 'Selected cycle' })).toHaveTextContent('1,234,567');
    await fireEvent.mouseEnter(quotas[0]);
    expect(quotas[0]).toHaveAttribute('aria-pressed', 'true');
    expect(tokens[0]).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('status')).toHaveTextContent('Unknown');
    expect(screen.getByRole('status').querySelectorAll('time')[1]).toHaveAttribute(
      'datetime',
      missing.endedAt,
    );
    await fireEvent.focus(tokens[1]);
    expect(quotas[1]).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('status')).toHaveTextContent('0%');
    expect(cycles).toEqual([latest, missing, zero]);
  });

  it.each([1, 5, 6, 7, 12, 14])(
    'shows only the latest six of %s cycles in chronological order without mutating history',
    (count) => {
      const history = Array.from({ length: count }, (_, index) => cycle(index + 1)).reverse();
      const cycles = Object.freeze(history) as unknown as ResetCycleUsage[];
      render(CycleUsageChart, { cycles });
      const expected = history.slice(0, 6).reverse();
      for (const chart of [quotaChart(), tokenChart()]) {
        const points = within(chart).getAllByRole('button');
        expect(points).toHaveLength(Math.min(count, 6));
        points.forEach((point, index) => {
          expect(point).toHaveAccessibleName(expect.stringContaining(expected[index].startedAt));
        });
        expect(points.at(-1)).toHaveAttribute('aria-pressed', 'true');
      }
      expect(screen.queryByRole('navigation')).not.toBeInTheDocument();
      expect(cycles).toEqual(history);
    },
  );

  it('does not fabricate chart cycles for empty history', () => {
    render(CycleUsageChart, { cycles: [] });
    expect(screen.queryByRole('group')).not.toBeInTheDocument();
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });

  it('bounds keyboard selection to the visible window and links both charts', async () => {
    render(CycleUsageChart, {
      cycles: Array.from({ length: 14 }, (_, index) => cycle(index + 1)).reverse(),
    });
    const tokens = within(tokenChart()).getAllByRole('button');
    const quotas = within(quotaChart()).getAllByRole('button');
    tokens[0].focus();
    await fireEvent.keyDown(tokens[0], { key: 'ArrowLeft' });
    await waitFor(() => expect(tokens[0]).toHaveFocus());
    expect(tokens[0]).toHaveAccessibleName(expect.stringContaining(cycle(9).startedAt));
    expect(quotas[0]).toHaveAttribute('aria-pressed', 'true');
    await fireEvent.keyDown(tokens[0], { key: 'End' });
    await waitFor(() => expect(tokens[5]).toHaveFocus());
    expect(quotas[5]).toHaveAttribute('aria-pressed', 'true');
    await fireEvent.keyDown(tokens[5], { key: 'ArrowRight' });
    await waitFor(() => expect(tokens[5]).toHaveFocus());
    await fireEvent.keyDown(tokens[5], { key: 'Home' });
    await waitFor(() => expect(tokens[0]).toHaveFocus());
    await fireEvent.keyDown(tokens[0], { key: 'ArrowRight' });
    await waitFor(() => expect(tokens[1]).toHaveFocus());
    expect(quotas[1]).toHaveAttribute('aria-pressed', 'true');
    await fireEvent.keyDown(tokens[1], { key: 'ArrowLeft' });
    await waitFor(() => expect(tokens[0]).toHaveFocus());
    await fireEvent.click(quotas[3]);
    expect(tokens[3]).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('status')).toHaveTextContent('14,808');
    expect(within(tokenChart()).getAllByRole('button')).toHaveLength(6);
  });

  it('follows the latest cycle through refreshes and newly arriving cycles', async () => {
    const history = Array.from({ length: 7 }, (_, index) => cycle(index + 1));
    const { rerender } = render(CycleUsageChart, { cycles: history });
    expect(screen.getByRole('status')).toHaveTextContent('8,638');
    const refreshed = history.map((entry) =>
      entry.startedAt === cycle(7).startedAt
        ? { ...entry, usage: { ...entry.usage, tokens: 70_000 } }
        : { ...entry },
    );
    await rerender({ cycles: refreshed });
    expect(screen.getByRole('status')).toHaveTextContent('70,000');
    await rerender({ cycles: [...refreshed, cycle(8)] });
    expect(within(tokenChart()).getAllByRole('button').at(-1)).toHaveAccessibleName(
      expect.stringContaining(cycle(8).startedAt),
    );
    expect(screen.getByRole('status')).toHaveTextContent('9,872');
    await fireEvent.click(within(quotaChart()).getAllByRole('button').at(-1)!);
    await rerender({ cycles: [...refreshed, cycle(8), cycle(9)] });
    expect(within(quotaChart()).getAllByRole('button').at(-1)).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    expect(screen.getByRole('status')).toHaveTextContent('11,106');
  });

  it('preserves a visible historical selection on refresh and resets only when it rolls out', async () => {
    const history = Array.from({ length: 7 }, (_, index) => cycle(index + 1));
    const { rerender } = render(CycleUsageChart, { cycles: history });
    await fireEvent.click(within(quotaChart()).getAllByRole('button')[1]);
    expect(screen.getByRole('status')).toHaveTextContent('3,702');
    const refreshed = history.map((entry) => ({
      ...entry,
      usage: { ...entry.usage, tokens: entry.usage.tokens + 1 },
    }));
    await rerender({ cycles: refreshed });
    expect(screen.getByRole('status')).toHaveTextContent('3,703');
    await rerender({ cycles: [...refreshed, cycle(8)] });
    expect(within(tokenChart()).getAllByRole('button')[0]).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('status')).toHaveTextContent('3,703');
    await rerender({ cycles: [...refreshed, cycle(8), cycle(9)] });
    expect(screen.getByRole('status')).toHaveTextContent('11,106');
    expect(within(tokenChart()).getAllByRole('button').at(-1)).toHaveAttribute(
      'aria-pressed',
      'true',
    );
  });

  it('keeps the complete history detail available below the rolling charts', () => {
    render(CycleUsageDetail, {
      title: 'Weekly API Value',
      cycles: Array.from({ length: 14 }, (_, index) => cycle(index + 1)).reverse(),
      top: 0,
      onEnter: () => {},
      onLeave: () => {},
    });
    expect(within(quotaChart()).getAllByRole('button')).toHaveLength(6);
    expect(screen.getAllByRole('button', { name: 'Show cycle model usage' })).toHaveLength(14);
  });

  it('keeps observed percentage and token totals unchanged when pricing display switches', async () => {
    const cycles = [cycle(2, 12.75, 9_876_543)];
    const props = {
      title: 'Weekly API Value',
      cycles,
      top: 0,
      onEnter: () => {},
      onLeave: () => {},
      credits: false,
    };
    const { rerender } = render(CycleUsageDetail, props);
    const quota = within(quotaChart()).getByRole('button').getAttribute('aria-label');
    const tokens = within(tokenChart()).getByRole('button').getAttribute('title');
    expect(screen.getByText('$3.00 used')).toBeVisible();
    await rerender({ ...props, credits: true });
    expect(screen.getByText('75 cr used')).toBeVisible();
    expect(within(quotaChart()).getByRole('button')).toHaveAccessibleName(quota!);
    expect(within(tokenChart()).getByRole('button')).toHaveAttribute('title', tokens);
  });

  it('preserves hover and focused reading lifetime while interacting with charts', async () => {
    vi.useFakeTimers();
    const current = cycle(2);
    render(UsageMetric, { label: 'Weekly API Value', period: current.usage, cycles: [current] });
    const trigger = screen.getByRole('button');
    await fireEvent.mouseEnter(trigger);
    await vi.advanceTimersByTimeAsync(400);
    const panel = screen.getByRole('tooltip');
    await fireEvent.mouseLeave(trigger);
    await fireEvent.mouseEnter(panel);
    const point = within(quotaChart()).getByRole('button');
    await fireEvent.mouseEnter(point);
    await vi.advanceTimersByTimeAsync(500);
    expect(panel).toBeVisible();
    point.focus();
    await fireEvent.focus(point);
    await fireEvent.mouseLeave(panel);
    await vi.advanceTimersByTimeAsync(500);
    expect(panel).toBeVisible();
    point.blur();
    await fireEvent.focusOut(panel, { relatedTarget: document.body });
    await vi.advanceTimersByTimeAsync(180);
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
  });

  it('localizes chart labels and unknown values including point tooltips', async () => {
    setLanguagePreference('simplifiedChinese');
    render(CycleUsageChart, { cycles: [{ ...cycle(2, null), endedAt: null }] });
    const stop = installLocalization();
    try {
      expect(screen.getByRole('group', { name: '重置周期图' })).toBeVisible();
      expect(screen.queryByRole('navigation')).not.toBeInTheDocument();
      const point = within(screen.getByRole('group', { name: '观测额度消耗（%）' })).getByRole(
        'button',
      );
      expect(point).toHaveAccessibleName(/当前 · 观测额度消耗（%）：未知/);
      expect(point).toHaveAttribute('title', expect.stringContaining('未知'));
      setLanguagePreference('english');
      await waitFor(() =>
        expect(point).toHaveAccessibleName(/Current · Observed quota consumption \(%\): Unknown/),
      );
    } finally {
      stop();
    }
  });
});
