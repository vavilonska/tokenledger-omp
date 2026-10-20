import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { providerCatalogIndex, settingsState } from '../test/appFixtures';
import TotalSpend from './TotalSpend.svelte';
import { renderTotalSpendShareCard } from './shareCard';
import { projectSpend } from './totalSpend';
import type { UsageHistory } from './types';

const mixedProviders = [10, 0].map((cost, index) => ({
  id: index === 0 ? 'claude' : 'codex',
  usage: {
    today: {
      tokens: 1_000_000,
      estimatedCostUsd: cost,
      costEstimated: false,
      estimateComplete: true,
    },
    yesterday: null,
    last30Days: null,
    daily: [],
    unknownModels: [],
  } satisfies UsageHistory,
}));

const settings = {
  ...settingsState.settings,
  costDisplay: 'apiUsd' as const,
  totalSpendPeriod: 'today' as const,
};

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe('Total Spend zero-cost consumers', () => {
  it('renders the blended $5/MTok center and the known zero provider row', () => {
    const { container } = render(TotalSpend, {
      providers: mixedProviders,
      settings: { ...settings, totalSpendMetric: 'costPerMillion' },
      catalog: providerCatalogIndex,
      onChange: vi.fn(),
      onShare: vi.fn(() => true),
    });
    expect(container.querySelector('.spend-ring__label strong')).toHaveTextContent('$5.00');
    expect(container.querySelector('.spend-ring__label')).toHaveAttribute(
      'data-tooltip',
      '$5.00/MTok',
    );
    expect(screen.getByText('$0.00/MTok')).toBeVisible();
    expect(container.querySelectorAll('.spend-ring__segment')).toHaveLength(1);
    expect(container.querySelector('.total-card__empty')).not.toBeInTheDocument();
  });

  it.each(['cost', 'costPerMillion'] as const)(
    'renders known all-zero %s data instead of the missing-data state',
    (metric) => {
      const { container } = render(TotalSpend, {
        providers: mixedProviders.slice(1),
        settings: { ...settings, totalSpendMetric: metric },
        catalog: providerCatalogIndex,
        onChange: vi.fn(),
        onShare: vi.fn(() => true),
      });
      expect(container.querySelector('.spend-ring__label strong')).toHaveTextContent(
        metric === 'cost' ? '$0' : '$0.00',
      );
      expect(container.querySelector('.spend-legend')).toHaveTextContent(
        metric === 'cost' ? '$0.00' : '$0.00/MTok',
      );
      expect(container.querySelector('.total-card__empty')).not.toBeInTheDocument();
      expect(container.querySelectorAll('.spend-ring__segment')).toHaveLength(0);
    },
  );

  it('keeps a known partial zero amount labelled partial, with no complete cost-per-token claim', () => {
    const providers = mixedProviders.slice(1).map((provider) => ({
      ...provider,
      usage: {
        ...provider.usage,
        today: { ...provider.usage.today, estimateComplete: false },
      },
    }));
    const { container, unmount } = render(TotalSpend, {
      providers,
      settings: { ...settings, totalSpendMetric: 'cost' },
      catalog: providerCatalogIndex,
      onChange: vi.fn(),
      onShare: vi.fn(() => true),
    });
    expect(screen.getByText('$0.00 partial')).toBeVisible();
    expect(container.querySelector('.spend-ring__label')).toHaveAttribute(
      'data-tooltip',
      expect.stringContaining('Partial estimate'),
    );
    unmount();
    render(TotalSpend, {
      providers,
      settings: { ...settings, totalSpendMetric: 'costPerMillion' },
      catalog: providerCatalogIndex,
      onChange: vi.fn(),
      onShare: vi.fn(() => true),
    });
    expect(screen.getByText('No cost-per-token data for this period')).toBeVisible();
    expect(screen.getByText('1M tokens')).toBeVisible();
  });

  it('exports the blended $5 rate and all-zero cost without missing-data text', () => {
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
      projection: projectSpend(mixedProviders, 'today', 'costPerMillion'),
      metric: 'costPerMillion',
      period: 'today',
    });
    expect(drawn).toContain('$5.00');
    expect(drawn).toContain('$0.00/MTok');
    drawn.length = 0;
    renderTotalSpendShareCard(providerCatalogIndex, {
      projection: projectSpend(mixedProviders.slice(1), 'today', 'cost'),
      metric: 'cost',
      period: 'today',
    });
    expect(drawn).toContain('$0');
    expect(drawn).toContain('$0.00');
    expect(drawn.join(' ')).not.toContain('No cost data');
  });
});
