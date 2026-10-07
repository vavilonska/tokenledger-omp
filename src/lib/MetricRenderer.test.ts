import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { codexState, providerCatalog, settingsState } from '../test/appFixtures';
import { ProviderCatalogIndex } from './metrics';
import MetricRenderer from './MetricRenderer.svelte';
import type { MetricDefinition } from './types';

afterEach(cleanup);

it('distinguishes a missing 5h window from a weekly window without attribution', () => {
  const definitions = structuredClone(providerCatalog);
  const provider = definitions.providers.find((item) => item.id === 'codex')!;
  const snapshot = structuredClone(codexState.snapshot!);
  snapshot.quotas = snapshot.quotas.filter((quota) => quota.id === 'weekly');
  snapshot.usage.sessionCycle = null;
  snapshot.usage.weeklyCycle = null;

  for (const period of ['sessionCycle', 'weeklyCycle'] as const) {
    const metric: MetricDefinition = {
      ...provider.metrics.find((item) => item.source.kind === 'usage')!,
      id: `test.${period}`,
      label: period,
      source: { kind: 'usage', period },
    };
    provider.metrics.push(metric);
    render(MetricRenderer, {
      layout: { id: metric.id, enabled: true, section: 'onDemand', pinned: false },
      snapshot,
      settings: { ...settingsState.settings, costDisplay: 'apiUsd' },
      now: Date.now(),
      catalog: new ProviderCatalogIndex(definitions),
      onSettingsChange: vi.fn(),
    });
  }

  expect(screen.getByRole('button', { name: 'Quota window not reported' })).toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'No usable session attribution' })).toBeInTheDocument();
});
