import { describe, expect, it } from 'vitest';
import { projectSpend } from './totalSpend';
import type { UsageHistory, UsagePeriod } from './types';

const empty: UsageHistory = {
  today: null,
  yesterday: null,
  last30Days: null,
  daily: [],
  unknownModels: [],
};

function usage(today: UsageHistory['today']): UsageHistory {
  return { ...empty, today };
}

describe('Total Spend projection', () => {
  it('keeps token-only Codex data available without inventing cost data', () => {
    const providers = [
      {
        id: 'codex',
        usage: usage({
          tokens: 164_800_000,
          estimatedCostUsd: null,
          costEstimated: true,
          estimateComplete: false,
        }),
      },
      { id: 'antigravity', usage: empty },
    ];

    expect(projectSpend(providers, 'today', 'cost').slices).toEqual([]);
    expect(projectSpend(providers, 'today', 'tokens')).toMatchObject({
      centerValue: 164_800_000,
      slices: [{ id: 'codex', value: 164_800_000 }],
    });
  });

  it('does not let a token-only provider erase another provider cost', () => {
    const providers = [
      {
        id: 'claude',
        usage: usage({
          tokens: 1_000_000,
          estimatedCostUsd: 4,
          costEstimated: true,
          estimateComplete: true,
        }),
      },
      {
        id: 'codex',
        usage: usage({
          tokens: 9_000_000,
          estimatedCostUsd: null,
          costEstimated: true,
          estimateComplete: false,
        }),
      },
    ];

    expect(projectSpend(providers, 'today', 'cost')).toMatchObject({
      centerValue: 4,
      slices: [{ id: 'claude', value: 4 }],
    });
  });

  it('calculates a blended cost per million instead of summing provider rates', () => {
    const providers = [
      {
        id: 'claude',
        usage: usage({
          tokens: 1_000_000,
          estimatedCostUsd: 10,
          costEstimated: true,
          estimateComplete: true,
        }),
      },
      {
        id: 'codex',
        usage: usage({
          tokens: 3_000_000,
          estimatedCostUsd: 60,
          costEstimated: true,
          estimateComplete: true,
        }),
      },
    ];

    expect(projectSpend(providers, 'today', 'costPerMillion').centerValue).toBe(17.5);
  });

  it('tracks local estimation independently from pricing coverage', () => {
    const providers = [
      {
        id: 'claude',
        usage: usage({
          tokens: 1_000,
          estimatedCostUsd: 2,
          costEstimated: true,
          estimateComplete: false,
        }),
      },
    ];

    expect(projectSpend(providers, 'today', 'cost')).toMatchObject({
      costEstimated: true,
      estimateComplete: false,
    });
    expect(projectSpend(providers, 'today', 'tokens')).toMatchObject({
      costEstimated: false,
      estimateComplete: false,
    });
  });

  it('includes known zero-cost tokens in the blended denominator ($5, not $10)', () => {
    const providers = [10, 0].map((cost, index) => ({
      id: `provider-${index}`,
      usage: usage({
        tokens: 1_000_000,
        estimatedCostUsd: cost,
        costEstimated: false,
        estimateComplete: true,
      }),
    }));
    expect(projectSpend(providers, 'today', 'cost')).toMatchObject({
      centerValue: 10,
      estimateComplete: true,
      slices: [{ value: 10 }, { value: 0 }],
    });
    expect(projectSpend(providers, 'today', 'costPerMillion')).toMatchObject({
      centerValue: 5,
      totalTokens: 2_000_000,
      estimateComplete: true,
      slices: [{ value: 10 }, { value: 0 }],
    });
  });

  it.each(['cost', 'costPerMillion'] as const)(
    'keeps all-zero %s data distinct from empty data',
    (metric) => {
      const free = {
        id: 'free',
        usage: usage({
          tokens: 1_000_000,
          estimatedCostUsd: 0,
          costEstimated: true,
          estimateComplete: true,
        }),
      };
      expect(projectSpend([free], 'today', metric)).toMatchObject({
        centerValue: 0,
        costEstimated: true,
        estimateComplete: true,
        slices: [{ id: 'free', value: 0 }],
      });
      expect(projectSpend([], 'today', metric)).toMatchObject({ centerValue: null, slices: [] });
      expect(projectSpend([{ id: 'empty', usage: empty }], 'today', metric)).toMatchObject({
        centerValue: null,
        slices: [],
      });
    },
  );

  it.each([null, undefined, NaN, Infinity, -Infinity, -1])(
    'does not treat invalid cost %s as known or zero',
    (estimatedCostUsd) => {
      const providers = [
        {
          id: 'unknown',
          usage: usage({
            tokens: 1_000_000,
            estimatedCostUsd,
            costEstimated: false,
            estimateComplete: true,
          } as UsagePeriod),
        },
      ];
      for (const metric of ['cost', 'costPerMillion'] as const) {
        expect(projectSpend(providers, 'today', metric)).toMatchObject({
          centerValue: null,
          estimateComplete: false,
          slices: [],
        });
      }
    },
  );

  it('keeps partial and unknown tokens outside the complete-cost blended denominator', () => {
    const providers = [
      { id: 'paid', tokens: 1_000_000, cost: 10, complete: true },
      { id: 'free', tokens: 1_000_000, cost: 0, complete: true },
      { id: 'partial', tokens: 8_000_000, cost: 20, complete: false },
      { id: 'unknown', tokens: 9_000_000, cost: null, complete: false },
      { id: 'partial-free', tokens: 7_000_000, cost: 0, complete: false },
    ].map(({ id, tokens, cost, complete }) => ({
      id,
      usage: usage({
        tokens,
        estimatedCostUsd: cost,
        costEstimated: true,
        estimateComplete: complete,
      }),
    }));
    expect(projectSpend(providers, 'today', 'cost')).toMatchObject({
      centerValue: 30,
      estimateComplete: false,
      slices: [{ id: 'partial' }, { id: 'paid' }, { id: 'free' }, { id: 'partial-free' }],
    });
    expect(projectSpend(providers, 'today', 'costPerMillion')).toMatchObject({
      centerValue: 5,
      totalTokens: 26_000_000,
      estimateComplete: false,
      slices: [
        { id: 'paid', value: 10 },
        { id: 'free', value: 0 },
      ],
    });
  });
});
