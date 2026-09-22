import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { costReading, costTooltip, creditHistory } from './costDisplay';
import UsageMetric from './UsageMetric.svelte';
import TotalSpend from './TotalSpend.svelte';
import { providerCatalogIndex, settingsState } from '../test/appFixtures';

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe('independent Codex credit display', () => {
  it('anchors credit tooltips to 2500 = 100 USD and preserves sub-credit amounts', () => {
    expect(costReading(100, true)).toBe('2.5K cr');
    expect(costTooltip(100, true)).toContain('2,500 cr ≈ $100.00 USD');
    expect(costReading(0.001, true)).toBe('0.03 cr');
    expect(costReading(100, false)).toBe('$100.00');
  });

  it('does not relabel API dollars in an older snapshot as credits', () => {
    expect(
      creditHistory({
        today: { tokens: 100, estimatedCostUsd: 5, costEstimated: true, estimateComplete: true },
        yesterday: null,
        last30Days: null,
        daily: [],
        unknownModels: [],
      }).today,
    ).toBeNull();
  });

  it('switches the overview using independent credit history and persists the preference', async () => {
    const period = {
      tokens: 100_000,
      estimatedCostUsd: 2,
      costEstimated: true,
      estimateComplete: true,
    };
    const creditUsage = {
      today: { ...period, estimatedCostUsd: 2.5 },
      yesterday: null,
      last30Days: null,
      daily: [],
      unknownModels: [],
    };
    const props = {
      providers: [{ id: 'codex', usage: { ...creditUsage, today: period, creditUsage } }],
      settings: { ...settingsState.settings, costDisplay: 'apiUsd' as const },
      catalog: providerCatalogIndex,
      onChange: vi.fn(),
      onShare: vi.fn(),
    };
    const view = render(TotalSpend, props);
    expect(screen.getAllByText('$2.00').length).toBeGreaterThan(0);
    await fireEvent.click(screen.getByRole('button', { name: 'Codex credits' }));
    expect(props.onChange).toHaveBeenCalledWith(
      expect.objectContaining({ costDisplay: 'codexCredits' }),
    );
    await view.rerender({ ...props, settings: { ...props.settings, costDisplay: 'codexCredits' } });
    expect(screen.getByText('62.5 cr')).toBeVisible();
    expect(screen.queryByText('$2.00')).not.toBeInTheDocument();
  });

  it('shows anchored dollars on credit usage and model hover details', async () => {
    vi.useFakeTimers();
    render(UsageMetric, {
      label: 'Today',
      credits: true,
      period: {
        tokens: 100_000,
        estimatedCostUsd: 2.5,
        costEstimated: true,
        estimateComplete: true,
        modelBreakdown: {
          sourceNote: 'Codex credit estimate',
          models: [{ model: 'gpt-6-astra', totalTokens: 100_000, costUsd: 2.5 }],
        },
      },
    });
    const reading = screen.getByRole('button', { name: '62.5 cr · 100K tokens' });
    expect(reading.getAttribute('data-tooltip')).toContain('$2.50 USD');
    await fireEvent.mouseEnter(reading);
    await vi.advanceTimersByTimeAsync(400);
    expect(screen.getByText('62.5 cr').getAttribute('title')).toContain('$2.50 USD');
  });
});
