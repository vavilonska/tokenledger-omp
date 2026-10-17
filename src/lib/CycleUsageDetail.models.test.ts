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
    {
      model: 'formerly-folded-model',
      totalTokens: 1_500,
      requestCount: 17,
      costUsd: 1,
      costComplete: true,
    },
    {
      model: 'Unattributed',
      totalTokens: 500,
      requestCount: null,
      costUsd: null,
      costComplete: false,
    },
    {
      model: 'main-model',
      totalTokens: 7_000,
      requestCount: 1234,
      costUsd: 99,
      costComplete: true,
    },
    {
      model: 'unpriced-model',
      totalTokens: 1_000,
      requestCount: 0,
      costUsd: null,
      costComplete: false,
    },
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

describe('cycle token and cost model disclosure', () => {
  it.each([false, true])(
    'keeps complete token-ranked models and separate cost shares, with credits=%s',
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
      const mainCells = within(rows[0]).getAllByRole('cell');
      expect(mainCells[0]).toHaveTextContent('7,000');
      expect(mainCells[0]).toHaveTextContent('70%');
      expect(mainCells[1]).toHaveTextContent(credits ? '2,475 cr' : '$99.00');
      expect(mainCells[1]).toHaveTextContent('99%');
      expect(mainCells[2]).toHaveTextContent('1,234');
      const foldedCells = within(rows[1]).getAllByRole('cell');
      expect(foldedCells[0]).toHaveTextContent('1,500');
      expect(foldedCells[0]).toHaveTextContent('15%');
      expect(foldedCells[1]).toHaveTextContent(credits ? '25 cr' : '$1.00');
      expect(foldedCells[1]).toHaveTextContent('1%');
      expect(foldedCells[2]).toHaveTextContent('17');
      expect(within(rows[2]).getAllByRole('cell').at(-1)).toHaveTextContent('0');
      expect(within(rows[3]).getAllByRole('cell').at(-1)).toHaveTextContent('Unknown');
      expect(detail).not.toHaveTextContent('Other');
      expect(within(detail).getAllByRole('columnheader')).toHaveLength(4);
      expect(within(rows[0]).getAllByRole('cell')[1].querySelector('[title]')).toHaveAttribute(
        'title',
        expect.stringContaining(credits ? '2,475 cr' : '$99.00'),
      );
      expect(within(rows[2]).getAllByRole('cell')[1]).toHaveTextContent(/^—\s*—$/);
      expect(within(rows[2]).getByTitle('Model cost is unknown.')).toHaveTextContent('—');
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
        modelUsage: [
          { model: 'zero-model', totalTokens: 0, requestCount: 0, costUsd: 0, costComplete: true },
        ],
      },
    ]);
    await fireEvent.focus(screen.getByRole('button', { name: 'Show cycle model usage' }));
    expect(within(screen.getAllByRole('row')[1]).getAllByRole('cell')[0]).toHaveTextContent('—');
  });

  it('opens historical cycles by keyboard without sharing the current cycle denominator', async () => {
    show([
      cycle,
      {
        ...cycle,
        startedAt: '2026-09-08T09:59:41Z',
        endedAt: cycle.startedAt,
        usage: { ...cycle.usage, tokens: 200 },
        modelUsage: [
          {
            model: 'historical-model',
            totalTokens: 50,
            requestCount: 3,
            costUsd: 10,
            costComplete: true,
          },
        ],
      },
    ]);
    const triggers = screen.getAllByRole('button', { name: 'Show cycle model usage' });
    await fireEvent.focus(triggers[0]);
    await fireEvent.focus(triggers[1]);
    expect(triggers[0]).toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByRole('dialog')).toHaveTextContent('historical-model');
    expect(screen.getByRole('dialog')).toHaveTextContent('25%');
    expect(screen.getByRole('dialog')).toHaveTextContent('10%');
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
      expect(screen.getByRole('columnheader', { name: /消费金额.*周期金额占比/ })).toBeVisible();
      expect(screen.getAllByTitle('模型金额未知。')[0]).toHaveTextContent('—');
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

  it.each([false, true])(
    'reads the selected basis once, including small amounts, credits=%s',
    async (credits) => {
      const apiCycle = {
        ...cycle,
        usage: { ...cycle.usage, estimatedCostUsd: 0.5 },
        modelUsage: [
          {
            model: 'basis-model',
            totalTokens: 1_000,
            requestCount: 1,
            costUsd: 0.125,
            costComplete: true,
          },
        ],
      };
      const creditCycle = {
        ...apiCycle,
        usage: { ...apiCycle.usage, estimatedCostUsd: 0.2 },
        modelUsage: [{ ...apiCycle.modelUsage[0], costUsd: 0.01 }],
      };
      show([credits ? creditCycle : apiCycle], credits);
      await fireEvent.focus(screen.getByRole('button', { name: 'Show cycle model usage' }));
      const cells = within(screen.getAllByRole('row')[1]).getAllByRole('cell');
      expect(cells[0]).toHaveTextContent('10%');
      expect(cells[1]).toHaveTextContent(credits ? '0.25 cr' : '$0.13');
      expect(cells[1]).toHaveTextContent(credits ? '5%' : '25%');
      expect(cells[1].querySelector('[title]')).toHaveAttribute(
        'title',
        expect.stringContaining(credits ? '0.25 cr' : '$0.13'),
      );
    },
  );

  it.each([
    {
      costUsd: null,
      costComplete: false,
      total: 100,
      complete: true,
      reason: 'Model cost is unknown.',
      amount: '—',
    },
    {
      costUsd: 2,
      costComplete: false,
      total: 100,
      complete: true,
      reason: 'Model cost is partial or unknown.',
      amount: '$2.00 partial',
    },
    {
      costUsd: 2,
      costComplete: true,
      total: 100,
      complete: false,
      reason: 'Cycle cost is partial or unknown.',
      amount: '$2.00',
    },
    {
      costUsd: 2,
      costComplete: true,
      total: null,
      complete: true,
      reason: 'Cycle cost is unknown.',
      amount: '$2.00',
    },
    {
      costUsd: 0,
      costComplete: true,
      total: 0,
      complete: true,
      reason: 'Cycle cost must be greater than zero.',
      amount: '$0.00',
    },
  ])(
    'explains unavailable cost shares: $reason',
    async ({ costUsd, costComplete, total, complete, reason, amount }) => {
      show([
        {
          ...cycle,
          usage: { ...cycle.usage, estimatedCostUsd: total, estimateComplete: complete },
          modelUsage: [
            { model: 'cost-state', totalTokens: 100, requestCount: 1, costUsd, costComplete },
          ],
        },
      ]);
      await fireEvent.focus(screen.getByRole('button', { name: 'Show cycle model usage' }));
      const cell = within(screen.getAllByRole('row')[1]).getAllByRole('cell')[1];
      expect(cell).toHaveTextContent(amount);
      expect(within(cell).getByTitle(reason)).toHaveTextContent('—');
      expect(cell).not.toHaveTextContent(/NaN|Infinity|%/);
    },
  );

  it('preserves token-only cached model rows without inventing a zero cost', async () => {
    show([{ ...cycle, modelUsage: [{ model: 'legacy-model', totalTokens: 50, requestCount: 3 }] }]);
    await fireEvent.focus(screen.getByRole('button', { name: 'Show cycle model usage' }));
    const cells = within(screen.getAllByRole('row')[1]).getAllByRole('cell');
    expect(cells[0]).toHaveTextContent('50');
    expect(cells[0]).toHaveTextContent('0.5%');
    expect(cells[1]).toHaveTextContent(/^—\s*—$/);
    expect(cells[1]).not.toHaveTextContent('$0');
    expect(cells[2]).toHaveTextContent('3');
  });

  it('shows an exactly known zero model cost as zero percent of a positive cycle', async () => {
    show([
      {
        ...cycle,
        modelUsage: [
          { model: 'free', totalTokens: 100, requestCount: 1, costUsd: 0, costComplete: true },
        ],
      },
    ]);
    await fireEvent.focus(screen.getByRole('button', { name: 'Show cycle model usage' }));
    const costCell = within(screen.getAllByRole('row')[1]).getAllByRole('cell')[1];
    expect(costCell).toHaveTextContent('$0.00');
    expect(costCell).toHaveTextContent('0%');
    expect(costCell).not.toHaveTextContent('partial');
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
