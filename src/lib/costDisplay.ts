import { formatMetricNumber } from './metricFormat';
import type { UsageHistory } from './types';

export const CREDITS_PER_USD = 25;
export const CREDIT_ANCHOR = '2,500 credits = $100';

export function costReading(usd: number | null | undefined, credits = false, full = false) {
  if (usd == null) return '—';
  if (!credits) return formatMetricNumber(usd, 'dollars', full ? 'full' : 'row');
  const value = usd * CREDITS_PER_USD;
  const number =
    full || Math.abs(value) < 1000
      ? value.toLocaleString('en-US', { maximumFractionDigits: 2 })
      : formatMetricNumber(value, 'count', 'row');
  return `${number} cr`;
}

export function costTooltip(usd: number | null | undefined, credits = false) {
  if (usd == null) return credits ? 'Credit estimate unavailable' : undefined;
  return credits
    ? `${costReading(usd, true, true)} ≈ ${formatMetricNumber(usd, 'dollars', 'full')} USD · ${CREDIT_ANCHOR} · Codex estimate`
    : `${formatMetricNumber(usd, 'dollars', 'full')} · API list price estimate`;
}

export function creditHistory(history: UsageHistory): UsageHistory {
  return (
    history.creditUsage ?? {
      today: null,
      yesterday: null,
      last30Days: null,
      daily: [],
      unknownModels: [],
    }
  );
}
