import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import UsageMetric from './UsageMetric.svelte';

describe('UsageMetric model detail', () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it('reveals the ranked real model names after the reference hover dwell', async () => {
    vi.useFakeTimers();
    render(UsageMetric, {
      label: 'Today',
      period: {
        tokens: 2_000,
        estimatedCostUsd: 0.04,
        costEstimated: true,
        estimateComplete: true,
        unknownModels: [],
        modelBreakdown: {
          sourceNote: 'From your Codex logs (estimated)',
          models: [
            { model: 'gpt-5.4', totalTokens: 1_100, costUsd: 0.03 },
            { model: 'gpt-5.3-codex', totalTokens: 900, costUsd: 0.01 },
          ],
        },
      },
    });

    const reading = screen.getByRole('button', { name: '$0.04 · 2K tokens' });
    await fireEvent.mouseEnter(reading);
    expect(screen.queryByRole('tooltip', { name: 'Today model usage' })).not.toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(400);

    const detail = screen.getByRole('tooltip', { name: 'Today model usage' });
    expect(detail).toHaveTextContent('gpt-5.4');
    expect(detail).toHaveTextContent('gpt-5.3-codex');
    expect(detail).toHaveTextContent('75%');
    expect(detail).toHaveTextContent('25%');
  });

  it('shows the unknown model warning without inventing a model breakdown', () => {
    render(UsageMetric, {
      label: 'Today',
      period: {
        tokens: 0,
        estimatedCostUsd: null,
        costEstimated: true,
        estimateComplete: false,
        unknownModels: ['future-unpriced-model'],
        modelBreakdown: null,
      },
    });

    expect(screen.getByLabelText('This period used a model with unknown pricing')).toHaveAttribute(
      'data-tooltip',
      'Unknown model found\n- future-unpriced-model',
    );
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
  });

  it('keeps incomplete cost text ordinary and reports local estimation separately', () => {
    render(UsageMetric, {
      label: 'Today',
      period: {
        tokens: 500,
        estimatedCostUsd: 0.03,
        costEstimated: true,
        estimateComplete: false,
        unknownModels: ['future-unpriced-model'],
        modelBreakdown: null,
      },
    });

    const reading = screen.getByRole('button', { name: '$0.03 · 500 tokens' });
    expect(reading).toHaveAttribute(
      'data-tooltip',
      '$0.03\n500 tokens\nEstimated locally, so it may be off',
    );
    expect(reading).not.toHaveTextContent('~');
    expect(screen.getByLabelText('This period used a model with unknown pricing')).toBeVisible();
  });

  it('compacts large row values while keeping exact tooltip figures', () => {
    render(UsageMetric, {
      label: 'Last 30 Days',
      period: {
        tokens: 1_506_025_363,
        estimatedCostUsd: 2_059.07,
        costEstimated: true,
        estimateComplete: true,
        unknownModels: [],
        modelBreakdown: null,
      },
    });

    expect(screen.getByRole('button', { name: '$2.1K · 1.5B tokens' })).toHaveAttribute(
      'data-tooltip',
      '$2,059.07\n1,506,025,363 tokens\nEstimated locally, so it may be off',
    );
  });

  it('lets the model detail replace the generic estimate tooltip', () => {
    render(UsageMetric, {
      label: 'Today',
      period: {
        tokens: 500,
        estimatedCostUsd: 0.03,
        costEstimated: true,
        estimateComplete: true,
        unknownModels: [],
        modelBreakdown: {
          sourceNote: 'From local logs (estimated)',
          models: [{ model: 'gpt-5.4', totalTokens: 500, costUsd: 0.03 }],
        },
      },
    });

    expect(screen.getByRole('button', { name: '$0.03 · 500 tokens' })).not.toHaveAttribute(
      'data-tooltip',
    );
  });

  it('shows inferred cycle quota value and both local clients in the detail', async () => {
    vi.useFakeTimers();
    render(UsageMetric, {
      label: 'Weekly API Value',
      period: {
        tokens: 3_000,
        estimatedCostUsd: 25,
        estimatedLimitUsd: 125,
        quotaUsedPercent: 20,
        costEstimated: true,
        estimateComplete: true,
        unknownModels: [],
        modelBreakdown: {
          sourceNote: 'Codex + Oh My Pi logs · API list price estimate',
          models: [
            {
              model: 'gpt-5.6-sol',
              totalTokens: 3_000,
              costUsd: 25,
              variants: [
                { model: 'Codex', totalTokens: 1_000, costUsd: 10 },
                { model: 'Oh My Pi', totalTokens: 2_000, costUsd: 15 },
              ],
            },
          ],
        },
      },
    });

    const reading = screen.getByRole('button', { name: '~$125.00 quota · $25.00' });
    await fireEvent.mouseEnter(reading);
    await vi.advanceTimersByTimeAsync(400);

    const detail = screen.getByRole('tooltip', { name: 'Weekly API Value model usage' });
    expect(detail).toHaveTextContent('Codex');
    expect(detail).toHaveTextContent('Oh My Pi');
    expect(detail).toHaveTextContent('API list price estimate');
  });

  it('shows server-observed reset cycles instead of fixed seven-day buckets', async () => {
    vi.useFakeTimers();
    const period = {
      tokens: 3_000,
      estimatedCostUsd: 25,
      estimatedLimitUsd: 125,
      quotaUsedPercent: 20,
      costEstimated: true,
      estimateComplete: true,
      unknownModels: [],
      modelBreakdown: {
        sourceNote: 'Codex + Oh My Pi logs · API list price estimate',
        models: [
          {
            model: 'gpt-5.6-sol',
            totalTokens: 3_000,
            costUsd: 25,
            variants: [
              { model: 'Codex', totalTokens: 1_000, costUsd: 10 },
              { model: 'Oh My Pi', totalTokens: 2_000, costUsd: 15 },
            ],
          },
        ],
      },
    };
    render(UsageMetric, {
      label: 'Weekly API Value',
      period,
      cycles: [
        {
          startedAt: '2026-08-30T06:36:48Z',
          endedAt: null,
          scheduledResetAt: '2026-09-06T06:36:48Z',
          usage: period,
        },
      ],
    });

    await fireEvent.mouseEnter(screen.getByRole('button', { name: '~$125.00 quota · $25.00' }));
    await vi.advanceTimersByTimeAsync(400);

    const detail = screen.getByRole('tooltip', {
      name: 'Weekly API Value reset cycle history',
    });
    expect(detail).toHaveTextContent('Actual reset cycles');
    expect(detail).toHaveTextContent('$25.00 used');
    expect(detail).toHaveTextContent('~$125.00 quota');
    expect(detail).toHaveTextContent('Codex $10.00');
    expect(detail).toHaveTextContent('Oh My Pi $15.00');
    expect(detail).toHaveTextContent('reset cause is not inferred');
  });
});
