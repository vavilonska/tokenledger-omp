import type { AppSettings, UsageHistory, UsagePeriod } from './types';

export interface SpendProvider {
  id: string;
  usage: UsageHistory;
}

export interface SpendSlice {
  id: string;
  value: number;
  period: UsagePeriod;
}

export interface SpendProjection {
  slices: SpendSlice[];
  centerValue: number | null;
  totalTokens: number;
  costEstimated: boolean;
  estimateComplete: boolean;
}

export function selectedPeriod(
  history: UsageHistory,
  selection: AppSettings['totalSpendPeriod'],
): UsagePeriod | null {
  if (selection === 'today') return history.today;
  if (selection === 'yesterday') return history.yesterday;
  return history.last30Days;
}

function isKnownCost(value: number | null | undefined): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

function valueFor(period: UsagePeriod, metric: AppSettings['totalSpendMetric']): number | null {
  if (metric === 'tokens') return period.tokens > 0 ? period.tokens : null;
  const cost = period.estimatedCostUsd;
  if (!isKnownCost(cost)) return null;
  if (metric === 'costPerMillion') {
    return period.estimateComplete && period.tokens > 0 ? (cost / period.tokens) * 1_000_000 : null;
  }
  return cost;
}

export function projectSpend(
  providers: SpendProvider[],
  periodSelection: AppSettings['totalSpendPeriod'],
  metric: AppSettings['totalSpendMetric'],
): SpendProjection {
  const periods = providers
    .map((provider) => selectedPeriod(provider.usage, periodSelection))
    .filter((period): period is UsagePeriod => period !== null);
  const totalTokens = periods.reduce((sum, period) => sum + period.tokens, 0);
  const estimateComplete = periods.every(
    (period) =>
      period.estimateComplete && (period.tokens === 0 || isKnownCost(period.estimatedCostUsd)),
  );
  const slices = providers
    .flatMap((provider) => {
      const period = selectedPeriod(provider.usage, periodSelection);
      if (!period) return [];
      const value = valueFor(period, metric);
      return value === null ? [] : [{ id: provider.id, value, period }];
    })
    .sort((left, right) => right.value - left.value || left.id.localeCompare(right.id));

  if (slices.length === 0) {
    return { slices, centerValue: null, totalTokens, costEstimated: false, estimateComplete };
  }

  const centerValue =
    metric === 'costPerMillion'
      ? (() => {
          const totalCost = slices.reduce(
            (sum, slice) => sum + (slice.period.estimatedCostUsd ?? 0),
            0,
          );
          const totalTokens = slices.reduce((sum, slice) => sum + slice.period.tokens, 0);
          return totalTokens > 0 ? (totalCost / totalTokens) * 1_000_000 : null;
        })()
      : slices.reduce((sum, slice) => sum + slice.value, 0);

  return {
    slices,
    centerValue,
    totalTokens,
    costEstimated: metric !== 'tokens' && slices.some((slice) => slice.period.costEstimated),
    estimateComplete,
  };
}

export function emptySpendMessage(metric: AppSettings['totalSpendMetric']) {
  if (metric === 'tokens') return 'No token data for this period';
  if (metric === 'costPerMillion') return 'No cost-per-token data for this period';
  return 'No cost data for this period';
}
