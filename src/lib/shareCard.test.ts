import { cleanup, render } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { codexState, providerCatalogIndex, settingsState } from '../test/appFixtures';
import { ProviderCatalogIndex } from './metrics';
import MetricRenderer from './MetricRenderer.svelte';
import {
  buildProviderShareRows as buildProviderShareRowsWithCatalog,
  providerIconPlacement,
  providerShareCardHeight,
  renderTotalSpendShareCard as renderTotalSpendShareCardWithCatalog,
  SHARE_CARD_SCALE,
  SHARE_CARD_WIDTH,
  TOTAL_SPEND_GEOMETRY,
  TOTAL_SPEND_OUTER_PADDING,
  TOTAL_SPEND_PERIOD_LABELS,
  totalSpendShareCardHeight,
  type ShareRow,
} from './shareCard';
import type {
  AppSettings,
  MetricDefinition,
  ProviderLayout,
  ProviderSnapshot,
  UsagePeriod,
} from './types';

function buildProviderShareRows(
  _providerId: string,
  snapshot: ProviderSnapshot,
  layout: ProviderLayout,
  settings: AppSettings,
  now: number,
) {
  return buildProviderShareRowsWithCatalog(providerCatalogIndex, snapshot, layout, settings, now);
}

function renderTotalSpendShareCard(
  options: Parameters<typeof renderTotalSpendShareCardWithCatalog>[1],
) {
  return renderTotalSpendShareCardWithCatalog(providerCatalogIndex, options);
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

interface CycleUsageFixture {
  catalog: ProviderCatalogIndex;
  snapshot: ProviderSnapshot;
  layout: ProviderLayout;
  settings: AppSettings;
  rows: () => ShareRow[];
}

function cycleUsageFixture(): CycleUsageFixture {
  const periods = [
    ['last30Days', '30 Days'],
    ['sessionCycle', '5h Usage'],
    ['weeklyCycle', 'Weekly Usage'],
  ] as const;
  const metrics: MetricDefinition[] = periods.map(([period, label]) => ({
    id: `codex.${period}`,
    label,
    source: { kind: 'usage', period },
    pinnable: true,
    defaultEnabled: true,
    defaultSection: 'alwaysVisible',
    defaultPinned: false,
    tray: null,
  }));
  const catalog = new ProviderCatalogIndex({
    providers: [{ ...providerCatalogIndex.provider('codex')!, metrics }],
  });
  const usage = (tokens: number, estimatedCostUsd: number): UsagePeriod => ({
    tokens,
    estimatedCostUsd,
    costEstimated: true,
    estimateComplete: true,
  });
  const snapshot = structuredClone(codexState.snapshot!);
  snapshot.notices = [];
  snapshot.usage = {
    today: null,
    yesterday: null,
    last30Days: usage(3000, 3),
    sessionCycle: usage(100, 0.1),
    weeklyCycle: usage(1000, 1),
    daily: [],
    unknownModels: [],
    creditUsage: {
      today: null,
      yesterday: null,
      last30Days: usage(4000, 4),
      sessionCycle: usage(200, 0.2),
      weeklyCycle: usage(800, 0.8),
      daily: [],
      unknownModels: [],
    },
  };
  const layout: ProviderLayout = {
    ...settingsState.settings.providers[0],
    metrics: metrics.map(({ id }) => ({
      id,
      enabled: true,
      section: 'alwaysVisible',
      pinned: false,
    })),
  };
  const settings: AppSettings = { ...settingsState.settings, costDisplay: 'apiUsd' };
  const rows = () => buildProviderShareRowsWithCatalog(catalog, snapshot, layout, settings, 0);
  return { catalog, snapshot, layout, settings, rows };
}

function expectDashboardReadings(fixture: CycleUsageFixture) {
  fixture.rows().forEach((row, index) => {
    if (row.kind !== 'text') throw new Error('Expected a usage text row');
    const view = render(MetricRenderer, {
      catalog: fixture.catalog,
      snapshot: fixture.snapshot,
      settings: fixture.settings,
      layout: fixture.layout.metrics[index],
      now: 0,
      onSettingsChange: vi.fn(),
    });
    expect(view.getByText(row.label)).toBeVisible();
    expect(view.getByRole('button', { name: row.value })).toBeVisible();
    view.unmount();
  });
}

describe('share card usage periods', () => {
  it('exports the monthly, session and weekly API readings shown on the dashboard', () => {
    const fixture = cycleUsageFixture();
    expect(fixture.rows()).toEqual([
      { kind: 'text', label: '30 Days', value: '$3.00 · 3K tokens', condensed: false },
      { kind: 'text', label: '5h Usage', value: '$0.10 · 100 tokens', condensed: true },
      { kind: 'text', label: 'Weekly Usage', value: '$1.00 · 1K tokens', condensed: true },
    ]);
    expectDashboardReadings(fixture);
  });

  it('uses independent credit periods and the dashboard credit labels', () => {
    const fixture = cycleUsageFixture();
    fixture.settings.costDisplay = 'codexCredits';
    expect(fixture.rows()).toEqual([
      { kind: 'text', label: '30 Days', value: '100 cr · 4K tokens', condensed: false },
      { kind: 'text', label: '5h Credits', value: '5 cr · 200 tokens', condensed: true },
      { kind: 'text', label: 'Weekly Credits', value: '20 cr · 800 tokens', condensed: true },
    ]);
    expectDashboardReadings(fixture);
  });

  it.each([null, undefined])('does not borrow monthly usage for missing cycles (%s)', (missing) => {
    const fixture = cycleUsageFixture();
    for (const display of ['apiUsd', 'codexCredits'] as const) {
      fixture.settings.costDisplay = display;
      const history =
        display === 'codexCredits' ? fixture.snapshot.usage.creditUsage! : fixture.snapshot.usage;
      history.sessionCycle = missing;
      history.weeklyCycle = missing;
      const value = display === 'codexCredits' ? 'Credit estimate unavailable' : 'No data';
      expect(fixture.rows().slice(1)).toMatchObject([{ value }, { value }]);
      expectDashboardReadings(fixture);
    }
  });

  it.each(['apiUsd', 'codexCredits'] as const)(
    'does not treat an unknown runtime period as monthly usage in %s',
    (display) => {
      const fixture = cycleUsageFixture();
      fixture.settings.costDisplay = display;
      fixture.catalog.metric('codex.sessionCycle')!.source = {
        kind: 'usage',
        period: 'futureCycle' as 'sessionCycle',
      };
      expect(fixture.rows()[1]).toMatchObject({
        label: '5h Usage',
        value: display === 'codexCredits' ? 'Credit estimate unavailable' : 'No data',
      });
      expectDashboardReadings(fixture);
    },
  );

  it.each(['apiUsd', 'codexCredits'] as const)(
    'matches inferred cycle quotas on the dashboard in %s',
    (display) => {
      const fixture = cycleUsageFixture();
      fixture.settings.costDisplay = display;
      const history =
        display === 'codexCredits' ? fixture.snapshot.usage.creditUsage! : fixture.snapshot.usage;
      history.sessionCycle!.estimatedLimitUsd = display === 'codexCredits' ? 3 : 2;
      history.weeklyCycle!.estimatedLimitUsd = display === 'codexCredits' ? 12 : 10;
      expect(fixture.rows().slice(1)).toMatchObject([
        { value: display === 'codexCredits' ? '~75 cr quota · 5 cr' : '~$2.00 quota · $0.10' },
        { value: display === 'codexCredits' ? '~300 cr quota · 20 cr' : '~$10.00 quota · $1.00' },
      ]);
      expectDashboardReadings(fixture);
    },
  );

  it.each(['apiUsd', 'codexCredits'] as const)(
    'retains partial estimates in %s cycles',
    (display) => {
      const fixture = cycleUsageFixture();
      fixture.settings.costDisplay = display;
      const history =
        display === 'codexCredits' ? fixture.snapshot.usage.creditUsage! : fixture.snapshot.usage;
      history.sessionCycle!.estimateComplete = false;
      history.weeklyCycle!.estimateComplete = false;
      history.sessionCycle!.estimatedLimitUsd = 3;
      history.weeklyCycle!.estimatedLimitUsd = 12;
      expect(fixture.rows().slice(1)).toMatchObject([
        {
          value:
            display === 'codexCredits' ? '5 cr partial · 200 tokens' : '$0.10 partial · 100 tokens',
        },
        {
          value:
            display === 'codexCredits' ? '20 cr partial · 800 tokens' : '$1.00 partial · 1K tokens',
        },
      ]);
      expectDashboardReadings(fixture);
    },
  );
});

describe('share card layout', () => {
  it('uses the authored width and 4x export scale', () => {
    expect(SHARE_CARD_WIDTH).toBe(360);
    expect(SHARE_CARD_SCALE).toBe(4);
  });

  it('aspect-fits non-100 provider viewBoxes in exported headers', () => {
    expect(providerIconPlacement('codex', 16, 16, 22)).toEqual({
      x: 16,
      y: 16,
      scale: 0.22,
    });
    expect(providerIconPlacement('devin', 16, 16, 22)).toEqual({
      x: 17.32,
      y: 16,
      scale: 0.44,
    });
    const opencode = providerIconPlacement('opencode', 16, 16, 22);
    expect(opencode.x).toBeCloseTo(18.2);
    expect(opencode.y).toBe(16);
    expect(opencode.scale).toBeCloseTo(22 / 30);
  });

  it('exports only the provider rows visible on the dashboard', () => {
    const snapshot = codexState.snapshot!;
    const settings = settingsState.settings;
    const collapsed = settings.providers[0];
    const collapsedRows = buildProviderShareRows(
      'codex',
      snapshot,
      collapsed,
      settings,
      Date.now(),
    );

    expect(collapsedRows.map((row) => row.kind)).toEqual(['quota', 'quota', 'trend']);

    const expandedRows = buildProviderShareRows(
      'codex',
      snapshot,
      { ...collapsed, expanded: true },
      settings,
      Date.now(),
    );
    expect(expandedRows.map((row) => row.kind)).toEqual([
      'quota',
      'quota',
      'trend',
      'quota',
      'quota',
      'text',
      'text',
      'text',
      'text',
      'text',
    ]);
    expect(expandedRows.slice(-3)).toMatchObject([
      { condensed: true },
      { condensed: true },
      { condensed: true },
    ]);
  });

  it('does not encode unknown pricing as an approximation prefix', () => {
    const snapshot = structuredClone(codexState.snapshot!);
    snapshot.usage.today = {
      tokens: 500,
      estimatedCostUsd: 0.03,
      costEstimated: true,
      estimateComplete: false,
      unknownModels: ['future-unpriced-model'],
    };
    const rows = buildProviderShareRows(
      'codex',
      snapshot,
      { ...settingsState.settings.providers[0], expanded: true },
      settingsState.settings,
      Date.now(),
    );

    expect(rows.find((row) => row.kind === 'text' && row.label === 'Today')).toMatchObject({
      value: '$0.03 partial · 500 tokens',
    });
  });

  it('keeps provider notices in exported cards', () => {
    const snapshot = structuredClone(codexState.snapshot!);
    snapshot.notices = [
      {
        id: 'rateLimited',
        title: 'Live usage paused',
        message: 'Retrying in about 5 minutes',
        tone: 'warning',
      },
    ];
    const rows = buildProviderShareRows(
      'codex',
      snapshot,
      settingsState.settings.providers[0],
      settingsState.settings,
      Date.now(),
    );

    expect(rows[0]).toMatchObject({
      kind: 'text',
      label: 'Live usage paused',
      value: 'Retrying in about 5 minutes',
    });
  });

  it('keeps provider-supplied count units in exported quota rows', () => {
    const snapshot = structuredClone(codexState.snapshot!);
    snapshot.quotas[0] = {
      ...snapshot.quotas[0],
      format: 'count',
      usedPercent: 25,
      usedValue: 25,
      limitValue: 100,
      unit: 'searches',
    };

    const rows = buildProviderShareRows(
      'codex',
      snapshot,
      settingsState.settings.providers[0],
      settingsState.settings,
      Date.now(),
    );

    expect(rows[0]).toMatchObject({ kind: 'quota', reading: '75 searches left' });
  });

  it('omits pacing copy for an unused non-session quota', () => {
    const now = Date.parse('2026-08-12T12:00:00Z');
    const snapshot = structuredClone(codexState.snapshot!);
    snapshot.quotas[1] = {
      ...snapshot.quotas[1],
      usedPercent: 0,
      resetsAt: new Date(now + (snapshot.quotas[1].periodSeconds * 1000) / 2).toISOString(),
    };
    const rows = buildProviderShareRows(
      'codex',
      snapshot,
      settingsState.settings.providers[0],
      { ...settingsState.settings, alwaysShowPacing: true },
      now,
    );

    expect(rows.find((row) => row.kind === 'quota' && row.label === 'Weekly')).toMatchObject({
      paceLabel: null,
    });
  });

  it('exports customizable status metrics as text rows', () => {
    const catalog = new ProviderCatalogIndex({
      providers: [
        {
          id: 'grok',
          displayName: 'Grok',
          shortName: 'G',
          fallbackEnabled: false,
          localUsageSourceNote: null,
          links: [],
          metrics: [
            {
              id: 'grok.payAsYouGo',
              label: 'Extra Usage',
              source: { kind: 'status', sourceId: 'payAsYouGo' },
              pinnable: true,
              defaultEnabled: true,
              defaultSection: 'alwaysVisible',
              defaultPinned: true,
              tray: { shortLabel: 'E', suffix: null },
            },
          ],
        },
      ],
    });
    const snapshot: ProviderSnapshot = {
      providerId: 'grok',
      plan: null,
      quotas: [],
      valueMetrics: [],
      statusMetrics: [
        {
          id: 'payAsYouGo',
          label: 'Extra Usage',
          text: '2500 cap',
          tone: 'positive',
        },
      ],
      notices: [],
      usage: { today: null, yesterday: null, last30Days: null, daily: [], unknownModels: [] },
      warnings: [],
      refreshedAt: '2026-07-18T00:00:00Z',
    };
    const layout: ProviderLayout = {
      id: 'grok',
      enabled: true,
      detected: true,
      expanded: false,
      metrics: [
        {
          id: 'grok.payAsYouGo',
          enabled: true,
          section: 'alwaysVisible',
          pinned: true,
        },
      ],
    };

    expect(
      buildProviderShareRowsWithCatalog(
        catalog,
        snapshot,
        layout,
        settingsState.settings,
        Date.now(),
      ),
    ).toEqual([{ kind: 'text', label: 'Extra Usage', value: '2500 cap', condensed: false }]);
  });

  it('keeps always-visible rows ahead of expanded rows like the dashboard', () => {
    const snapshot = codexState.snapshot!;
    const settings = settingsState.settings;
    const layout = settings.providers[0];
    const metric = (id: string) => layout.metrics.find((item) => item.id === id)!;
    const interleaved = {
      ...layout,
      expanded: true,
      metrics: [
        metric('codex.today'),
        metric('codex.session'),
        metric('codex.yesterday'),
        metric('codex.weekly'),
      ],
    };

    const rows = buildProviderShareRows('codex', snapshot, interleaved, settings, Date.now());
    expect(rows.map((row) => row.label)).toEqual(['Session', 'Weekly', 'Today', 'Yesterday']);
  });

  it('grows provider exports with content instead of enforcing a minimum canvas', () => {
    const settings = settingsState.settings;
    const layout = { ...settings.providers[0], expanded: true };
    const rows = buildProviderShareRows(
      'codex',
      codexState.snapshot!,
      layout,
      settings,
      Date.now(),
    );

    expect(providerShareCardHeight(rows)).toBeGreaterThan(
      providerShareCardHeight(rows.slice(0, 1)),
    );
    expect(providerShareCardHeight([])).toBeLessThan(providerShareCardHeight(rows));
  });

  it('keeps Total Spend to the period switcher and usage body', () => {
    expect(TOTAL_SPEND_PERIOD_LABELS).toEqual(['Today', 'Yesterday', '30 Days']);
    expect(TOTAL_SPEND_OUTER_PADDING).toBe(10);
    expect(TOTAL_SPEND_GEOMETRY).toMatchObject({
      width: 320,
      switcherHeight: 27,
      ringDiameter: 104,
      legendGap: 18,
    });
    expect(totalSpendShareCardHeight()).toBe(187);
  });

  it('renders the selected-period spend projection at share-card resolution', () => {
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
      quadraticCurveTo: vi.fn(),
      closePath: vi.fn(),
      stroke: vi.fn(),
      measureText: (value: string) => ({ width: value.length * 6 }),
      fillText: (value: string) => drawn.push(value),
      textAlign: 'left',
      textBaseline: 'alphabetic',
      fillStyle: '',
      strokeStyle: '',
      font: '',
      lineWidth: 1,
      lineCap: 'butt',
    };
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(
      context as unknown as CanvasRenderingContext2D,
    );

    const canvas = renderTotalSpendShareCard({
      projection: {
        slices: [
          {
            id: 'codex',
            value: 12,
            period: {
              tokens: 1_000_000,
              estimatedCostUsd: 12,
              costEstimated: true,
              estimateComplete: true,
            },
          },
        ],
        centerValue: 12,
        totalTokens: 1_000_000,
        costEstimated: true,
        estimateComplete: true,
      },
      metric: 'cost',
      period: 'last30Days',
    });

    expect(canvas.width).toBe(TOTAL_SPEND_GEOMETRY.width * SHARE_CARD_SCALE);
    expect(canvas.height).toBe(totalSpendShareCardHeight() * SHARE_CARD_SCALE);
    expect(drawn).toEqual(
      expect.arrayContaining(['Today', 'Yesterday', '30 Days', 'Codex', 'dollars']),
    );
  });
});
