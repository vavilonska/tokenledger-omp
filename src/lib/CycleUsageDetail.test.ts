import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';
import CycleUsageDetail from './CycleUsageDetail.svelte';
import type { ResetCycleUsage } from './types';

afterEach(cleanup);

const cycle: ResetCycleUsage = {
  startedAt: '2026-09-15T09:59:41Z',
  endedAt: null,
  scheduledResetAt: '2026-09-22T09:59:41Z',
  usage: {
    tokens: 3650,
    estimatedCostUsd: 36.5,
    estimatedLimitUsd: 182.5,
    quotaUsedPercent: 20,
    costEstimated: true,
    estimateComplete: true,
    unknownModels: [],
    modelBreakdown: {
      sourceNote: 'Codex + Oh My Pi logs',
      models: [
        {
          model: 'gpt-6-astra',
          totalTokens: 3600,
          costUsd: 36,
          variants: [
            { model: 'Codex', totalTokens: 1000, costUsd: 10 },
            { model: 'Oh My Pi', totalTokens: 2600, costUsd: 26 },
          ],
        },
        {
          model: 'Other',
          totalTokens: 50,
          costUsd: 0.5,
          variants: [
            { model: 'gpt-5.6-luna · Oh My Pi', totalTokens: 30, costUsd: 0.3 },
            { model: 'gpt-5.6-luna · Codex', totalTokens: 20, costUsd: 0.2 },
          ],
        },
      ],
    },
  },
};

describe('reset cycle source totals', () => {
  it.each([
    { credits: false, omp: 'Oh My Pi $26.30', codex: 'Codex $10.20', total: '$36.50 used' },
    { credits: true, omp: 'Oh My Pi 657.5 cr', codex: 'Codex 255 cr', total: '912.5 cr used' },
  ])('includes Oh My Pi and folded sources with credits=$credits', (example) => {
    const { container } = render(CycleUsageDetail, {
      title: 'Weekly API Value',
      cycles: [cycle],
      credits: example.credits,
      top: 0,
      onEnter: () => {},
      onLeave: () => {},
    });
    expect(screen.getByText(example.omp)).toBeVisible();
    expect(screen.getByText(example.codex)).toBeVisible();
    expect(screen.getByText(example.total)).toBeVisible();
    expect(container.querySelectorAll('.cycle-sources span')).toHaveLength(2);
  });
});
