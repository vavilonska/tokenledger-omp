import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import CycleUsageDetail from './CycleUsageDetail.svelte';
import UsageMetric from './UsageMetric.svelte';
import { installLocalization, setLanguagePreference } from './localization';
import type { ResetCycleUsage } from './types';

const cycle: ResetCycleUsage = {
  startedAt: '2026-09-15T09:59:41Z',
  endedAt: null,
  scheduledResetAt: '2026-09-22T09:59:41Z',
  usage: {
    tokens: 10_000,
    estimatedCostUsd: 100,
    costEstimated: true,
    estimateComplete: true,
    modelBreakdown: {
      sourceNote: 'Local logs',
      models: [
        { model: 'main-model', totalTokens: 7_000, costUsd: 99 },
        { model: 'Other', totalTokens: 3_000, costUsd: 1 },
      ],
    },
  },
  modelUsage: [
    { model: 'formerly-folded-model', totalTokens: 1_500, requestCount: 17 },
    { model: 'Unattributed', totalTokens: 500, requestCount: null },
    { model: 'main-model', totalTokens: 7_000, requestCount: 1234 },
    { model: 'unpriced-model', totalTokens: 1_000, requestCount: 0 },
  ],
};

function show(cycles = [cycle], credits = false) {
  return render(CycleUsageDetail, {
    title: 'Weekly API Value',
    cycles,
    credits,
    top: 80,
    onEnter: vi.fn(),
    onLeave: vi.fn(),
  });
}

afterEach(() => {
  cleanup();
  setLanguagePreference('english');
  vi.useRealTimers();
});

describe('cycle token model disclosure', () => {
  it.each([false, true])(
    'uses complete token-ranked models, not cost shares, with credits=%s',
    async (credits) => {
      show([cycle], credits);
      const trigger = screen.getByRole('button', { name: 'Show cycle model usage' });
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
      await fireEvent.mouseEnter(trigger);
      const detail = screen.getByRole('dialog', { name: 'Cycle model usage' });
      const rows = within(detail).getAllByRole('row').slice(1);
      expect(rows.map((row) => within(row).getByRole('rowheader').textContent)).toEqual([
        'main-model',
        'formerly-folded-model',
        'unpriced-model',
        'Unattributed',
      ]);
      expect(
        within(rows[0])
          .getAllByRole('cell')
          .map((cell) => cell.textContent),
      ).toEqual(['7,000', '70%', '1,234']);
      expect(
        within(rows[1])
          .getAllByRole('cell')
          .map((cell) => cell.textContent),
      ).toEqual(['1,500', '15%', '17']);
      expect(within(rows[2]).getAllByRole('cell').at(-1)).toHaveTextContent('0');
      expect(within(rows[3]).getAllByRole('cell').at(-1)).toHaveTextContent('Unknown');
      expect(detail).not.toHaveTextContent('Other');
      await fireEvent.mouseLeave(trigger);
      await fireEvent.mouseEnter(detail);
      expect(detail).toBeVisible();
      expect(trigger).toHaveAttribute('aria-expanded', 'true');
    },
  );

  it.each([undefined, null])(
    'keeps missing legacy modelUsage=%s explicitly unknown',
    async (modelUsage) => {
      show([{ ...cycle, modelUsage }]);
      await fireEvent.focus(screen.getByRole('button', { name: 'Show cycle model usage' }));
      expect(screen.getByRole('dialog')).toHaveTextContent(
        'Model usage is unknown for this cycle.',
      );
      expect(screen.queryByRole('table')).not.toBeInTheDocument();
    },
  );

  it('distinguishes known-empty usage and an undefined zero-token share', async () => {
    show([{ ...cycle, usage: { ...cycle.usage, tokens: 0 }, modelUsage: [] }]);
    const trigger = screen.getByRole('button', { name: 'Show cycle model usage' });
    await fireEvent.focus(trigger);
    expect(screen.getByRole('dialog')).toHaveTextContent('No model usage in this cycle.');
    await fireEvent.keyDown(trigger, { key: 'Escape' });
    expect(trigger).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    cleanup();
    show([
      {
        ...cycle,
        usage: { ...cycle.usage, tokens: 0 },
        modelUsage: [{ model: 'zero-model', totalTokens: 0, requestCount: 0 }],
      },
    ]);
    await fireEvent.focus(screen.getByRole('button', { name: 'Show cycle model usage' }));
    expect(within(screen.getAllByRole('row')[1]).getAllByRole('cell')[1]).toHaveTextContent('—');
  });

  it('opens historical cycles by keyboard without sharing the current cycle denominator', async () => {
    show([
      cycle,
      {
        ...cycle,
        startedAt: '2026-09-08T09:59:41Z',
        endedAt: cycle.startedAt,
        usage: { ...cycle.usage, tokens: 200 },
        modelUsage: [{ model: 'historical-model', totalTokens: 50, requestCount: 3 }],
      },
    ]);
    const triggers = screen.getAllByRole('button', { name: 'Show cycle model usage' });
    await fireEvent.focus(triggers[0]);
    await fireEvent.focus(triggers[1]);
    expect(triggers[0]).toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByRole('dialog')).toHaveTextContent('historical-model');
    expect(screen.getByRole('dialog')).toHaveTextContent('25%');
    expect(screen.getByRole('dialog')).not.toHaveTextContent('main-model');
  });

  it('returns focus to the token trigger when Escape closes the reading panel', async () => {
    show();
    const trigger = screen.getByRole('button', { name: 'Show cycle model usage' });
    await fireEvent.focus(trigger);
    const panel = screen.getByRole('dialog', { name: 'Cycle model usage' });
    panel.focus();
    await fireEvent.keyDown(panel, { key: 'Escape' });
    expect(trigger).toHaveFocus();
    expect(trigger).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('localizes dynamically opened tables and unknown request counts', async () => {
    setLanguagePreference('simplifiedChinese');
    show();
    const stop = installLocalization();
    try {
      await fireEvent.focus(screen.getByRole('button', { name: '显示周期模型用量' }));
      await waitFor(() =>
        expect(screen.getByRole('dialog', { name: '周期模型用量' })).toHaveTextContent(
          '周期 Token 占比',
        ),
      );
      expect(screen.getByRole('columnheader', { name: '请求数' })).toBeVisible();
      expect(screen.getByRole('rowheader', { name: '未归属' }).closest('tr')).toHaveTextContent(
        '未知',
      );
      setLanguagePreference('english');
      expect(screen.getByRole('columnheader', { name: 'Requests' })).toBeVisible();
    } finally {
      stop();
    }
  });

  it('keeps the parent open across mouse transfer and focused reading, then closes after exit', async () => {
    vi.useFakeTimers();
    render(UsageMetric, { label: 'Weekly API Value', period: cycle.usage, cycles: [cycle] });
    const reading = screen.getByRole('button');
    await fireEvent.mouseEnter(reading);
    await vi.advanceTimersByTimeAsync(400);
    const card = screen.getByRole('tooltip', { name: 'Weekly API Value reset cycle history' });
    await fireEvent.mouseLeave(reading);
    await fireEvent.mouseEnter(card);
    const trigger = within(card).getByRole('button', { name: 'Show cycle model usage' });
    await fireEvent.mouseEnter(trigger);
    await fireEvent.mouseLeave(trigger);
    await fireEvent.mouseEnter(screen.getByRole('dialog'));
    await vi.advanceTimersByTimeAsync(500);
    expect(card).toBeVisible();
    trigger.focus();
    await fireEvent.focus(trigger);
    await fireEvent.mouseLeave(card);
    await vi.advanceTimersByTimeAsync(500);
    expect(card).toBeVisible();
    trigger.blur();
    await fireEvent.focusOut(card, { relatedTarget: document.body });
    await vi.advanceTimersByTimeAsync(180);
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
  });
});
