import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { codexState, providerCatalogIndex, settingsState } from '../test/appFixtures';
import CycleUsageDetail from './CycleUsageDetail.svelte';
import TotalSpend from './TotalSpend.svelte';
import UsageMetric from './UsageMetric.svelte';
import { costReading } from './costDisplay';
import { buildProviderShareRows, renderTotalSpendShareCard } from './shareCard';
import { projectSpend } from './totalSpend';
import type { AppSettings, UsageHistory, UsagePeriod } from './types';

const unknown: UsagePeriod = {
  tokens: 2_000,
  estimatedCostUsd: null,
  costEstimated: true,
  estimateComplete: false,
  unknownModels: [],
  modelBreakdown: {
    sourceNote: 'Local request speed unavailable',
    models: [
      {
        model: 'gpt-6.1-sol',
        totalTokens: 2_000,
        costUsd: null,
        variants: [{ model: 'Oh My Pi', totalTokens: 2_000, costUsd: null }],
      },
    ],
  },
};

function history(period: UsagePeriod): UsageHistory {
  return { today: period, yesterday: period, last30Days: period, daily: [], unknownModels: [] };
}

function providers(period: UsagePeriod) {
  const usage = history(period);
  return [{ id: 'codex', usage: { ...usage, creditUsage: usage } }];
}

function renderSpend(
  period: UsagePeriod,
  credits: boolean,
  metric: AppSettings['totalSpendMetric'] = 'cost',
) {
  return render(TotalSpend, {
    providers: providers(period),
    settings: {
      ...settingsState.settings,
      costDisplay: credits ? 'codexCredits' : 'apiUsd',
      totalSpendPeriod: 'today',
      totalSpendMetric: metric,
    },
    catalog: providerCatalogIndex,
    onChange: vi.fn(),
    onShare: vi.fn(() => true),
  });
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe.each([false, true])('incomplete pricing consumers (credits=%s)', (credits) => {
  it('keeps unknown-speed tokens visible without claiming an unknown model', () => {
    render(UsageMetric, { label: 'Today', period: unknown, credits });
    const reading = screen.getByRole('button', { name: '2K tokens' });
    expect(reading).toHaveAttribute('data-tooltip', expect.stringContaining('request speed'));
    expect(
      screen.queryByLabelText('This period used a model with unknown pricing'),
    ).not.toBeInTheDocument();
  });

  it('shows complete cache evidence independently of unknown-speed prices in both pricing bases', async () => {
    vi.useFakeTimers();
    const period: UsagePeriod = {
      ...unknown,
      modelBreakdown: {
        sourceNote: 'Local request speed unavailable',
        models: [
          {
            model: 'gpt-6.1-sol',
            totalTokens: 2_000,
            costUsd: null,
            cacheReadTokens: 100,
            promptTokens: 200,
            variants: [
              {
                model: 'Oh My Pi',
                totalTokens: 2_000,
                costUsd: null,
                cacheReadTokens: 100,
                promptTokens: 200,
              },
            ],
          },
        ],
      },
    };
    render(UsageMetric, { label: 'Today', period, credits });
    await fireEvent.mouseEnter(screen.getByRole('button', { name: '2K tokens' }));
    await vi.advanceTimersByTimeAsync(400);
    const detail = screen.getByRole('tooltip');
    expect(detail.querySelector('.model-usage-cache-total')).toHaveTextContent('50%');
    expect(detail.querySelector('.model-usage-variant-cache')).toHaveTextContent('50%');
    expect(detail).toHaveTextContent('Partial estimate');
  });

  it('passes incomplete coverage into model details and uses token shares', async () => {
    vi.useFakeTimers();
    const period: UsagePeriod = {
      ...unknown,
      estimatedCostUsd: 10,
      estimatedLimitUsd: 100,
      modelBreakdown: {
        sourceNote: 'Local usage',
        models: [
          { model: 'gpt-6.1-sol', totalTokens: 1_000, costUsd: 9 },
          { model: 'gpt-6-astra', totalTokens: 1_000, costUsd: 1 },
        ],
      },
    };
    render(UsageMetric, { label: 'Weekly', period, credits });
    const reading = screen.getByRole('button', {
      name: `${costReading(10, credits)} partial · 2K tokens`,
    });
    expect(reading).not.toHaveTextContent('quota');
    await fireEvent.mouseEnter(reading);
    await vi.advanceTimersByTimeAsync(400);
    const detail = screen.getByRole('tooltip', { name: 'Weekly model usage' });
    expect(detail).toHaveTextContent('Shares use tokens');
    expect(screen.getAllByText('50%')).toHaveLength(2);
    expect(detail).toHaveTextContent(`${costReading(9, credits)} partial`);
    expect(detail).not.toHaveTextContent('90%');
  });

  it('renders unknown cycle sources as unavailable, not zero', () => {
    render(CycleUsageDetail, {
      title: 'Weekly',
      credits,
      top: 8,
      onEnter: vi.fn(),
      onLeave: vi.fn(),
      cycles: [
        {
          startedAt: '2026-10-01T00:00:00Z',
          scheduledResetAt: '2026-10-08T00:00:00Z',
          usage: { ...unknown, estimatedLimitUsd: 100 },
        },
      ],
    });
    expect(screen.getByText('Oh My Pi —')).toBeVisible();
    expect(screen.getByText('— used')).toBeVisible();
    expect(screen.getByText('quota unavailable')).toBeVisible();
    expect(screen.getByText('2K tokens')).toBeVisible();
  });

  it('retains mixed cycle subtotals while marking source coverage partial', () => {
    const period = structuredClone(unknown);
    period.estimatedCostUsd = 2;
    period.modelBreakdown!.models.push({
      model: 'Other',
      totalTokens: 100,
      costUsd: 2,
      variants: [{ model: 'gpt-6-astra · Oh My Pi', totalTokens: 100, costUsd: 2 }],
    });
    render(CycleUsageDetail, {
      title: 'Weekly',
      credits,
      top: 8,
      onEnter: vi.fn(),
      onLeave: vi.fn(),
      cycles: [
        {
          startedAt: '2026-10-01T00:00:00Z',
          scheduledResetAt: '2026-10-08T00:00:00Z',
          usage: period,
        },
      ],
    });
    expect(screen.getByText(`Oh My Pi ${costReading(2, credits)} partial`)).toBeVisible();
    expect(screen.getByText(`${costReading(2, credits)} partial used`)).toBeVisible();
  });

  it('retains tokens in the cost overview when every request is unpriced', () => {
    renderSpend(unknown, credits);
    expect(screen.getByText('2K tokens')).toBeVisible();
    expect(screen.getByText('No cost data for this period')).toBeVisible();
    expect(
      screen.getByText(credits ? 'Partial credit estimate' : 'Partial cost estimate'),
    ).toBeVisible();
  });

  it('labels the overview subtotal and provider legend as partial', () => {
    renderSpend({ ...unknown, estimatedCostUsd: 2 }, credits);
    expect(screen.getByText(`${costReading(2, credits)} partial`)).toBeVisible();
    expect(
      screen.getByText(credits ? 'Partial credit estimate' : 'Partial cost estimate'),
    ).toBeVisible();
  });

  it('does not present partial cost divided by all tokens as a rate', () => {
    renderSpend({ ...unknown, estimatedCostUsd: 2 }, credits, 'costPerMillion');
    expect(screen.getByText('No cost-per-token data for this period')).toBeVisible();
    expect(screen.getByText('Average covers fully priced providers only')).toBeVisible();
    expect(screen.getByText('2K tokens')).toBeVisible();
  });

  it('exports tokens and explicitly partial provider amounts', () => {
    const snapshot = structuredClone(codexState.snapshot!);
    snapshot.usage = providers(unknown)[0].usage;
    const settings = {
      ...settingsState.settings,
      costDisplay: credits ? ('codexCredits' as const) : ('apiUsd' as const),
    };
    const rows = () =>
      buildProviderShareRows(
        providerCatalogIndex,
        snapshot,
        { ...settings.providers[0], expanded: true },
        settings,
        Date.now(),
      );
    expect(rows().find((row) => row.kind === 'text' && row.label === 'Today')).toMatchObject({
      value: '2K tokens',
    });
    snapshot.usage.today = { ...unknown, estimatedCostUsd: 2 };
    snapshot.usage.creditUsage = history(snapshot.usage.today);
    expect(rows().find((row) => row.kind === 'text' && row.label === 'Today')).toMatchObject({
      value: `${costReading(2, credits)} partial · 2K tokens`,
    });
  });
});

describe('overview coverage and exported summaries', () => {
  it('counts omitted unpriced providers in coverage without erasing known cost', () => {
    const mixed = [
      ...providers(unknown),
      {
        id: 'claude',
        usage: history({
          ...unknown,
          tokens: 1_000_000,
          estimatedCostUsd: 4,
          estimateComplete: true,
        }),
      },
    ];
    expect(projectSpend(mixed, 'today', 'cost')).toMatchObject({
      centerValue: 4,
      totalTokens: 1_002_000,
      estimateComplete: false,
    });
    expect(projectSpend(mixed, 'today', 'costPerMillion')).toMatchObject({
      centerValue: 4,
      estimateComplete: false,
      slices: [{ id: 'claude', value: 4 }],
    });
    expect(
      projectSpend(providers({ ...unknown, estimatedCostUsd: 2 }), 'today', 'costPerMillion'),
    ).toMatchObject({ centerValue: null, estimateComplete: false, slices: [] });
  });

  it.each(['cost', 'costPerMillion'] as const)(
    'exports %s coverage and token-only fallback',
    (metric) => {
      const drawn: string[] = [];
      const context = {
        scale: vi.fn(),
        fillRect: vi.fn(),
        beginPath: vi.fn(),
        roundRect: vi.fn(),
        fill: vi.fn(),
        arc: vi.fn(),
        moveTo: vi.fn(),
        lineTo: vi.fn(),
        closePath: vi.fn(),
        quadraticCurveTo: vi.fn(),
        measureText: (value: string) => ({ width: value.length * 6 }),
        fillText: (value: string) => drawn.push(value),
      };
      vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(
        context as unknown as CanvasRenderingContext2D,
      );
      renderTotalSpendShareCard(providerCatalogIndex, {
        projection: projectSpend(providers(unknown), 'today', metric),
        metric,
        period: 'today',
      });
      expect(drawn).toContain('2K tokens');
      drawn.length = 0;
      const mixed = [
        ...providers(unknown),
        {
          id: 'claude',
          usage: history({
            ...unknown,
            tokens: 1_000_000,
            estimatedCostUsd: 4,
            estimateComplete: true,
          }),
        },
      ];
      renderTotalSpendShareCard(providerCatalogIndex, {
        projection: projectSpend(mixed, 'today', metric),
        metric,
        period: 'today',
      });
      expect(drawn).toContain(metric === 'cost' ? 'Partial estimate' : 'Priced subset only');
    },
  );
});
