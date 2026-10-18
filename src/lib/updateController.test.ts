import { describe, expect, it } from 'vitest';
import { nextUpdateLabel, updateFailure } from './updateController.svelte';

describe('update controller helpers', () => {
  it('normalizes structured and unknown update failures', () => {
    expect(
      updateFailure(
        { code: 'download_forbidden', message: 'Download refused.', retryable: false },
        'fallback',
      ),
    ).toEqual({
      code: 'download_forbidden',
      message: 'Download refused.',
      action: 'Try again later.',
      retryable: false,
    });
    expect(updateFailure(new Error('Update service unavailable.'), 'Safe fallback').message).toBe(
      'Update service unavailable.',
    );
  });

  it('formats the backend deadline without adding a fixed refresh interval', () => {
    const now = Date.parse('2026-07-13T12:00:00Z');
    expect(nextUpdateLabel('2026-07-13T12:05:00Z', now)).toBe('Next update in 5m');
    expect(nextUpdateLabel('2026-07-13T12:01:00Z', now)).toBe('Next update in 1m');
    expect(nextUpdateLabel('2026-07-13T12:00:15Z', now)).toBe('Next update in 15s');
    expect(nextUpdateLabel('2026-07-13T12:00:00.001Z', now)).toBe('Next update in 1s');
    expect(nextUpdateLabel('2026-07-13T11:59:00Z', now)).toBe('Next update in 0s');
    expect(nextUpdateLabel('invalid', now)).toBe('Next update unavailable');
    expect(nextUpdateLabel(null, now)).toBe('Waiting for first update');
    expect(nextUpdateLabel(undefined, now)).toBe('Waiting for first update');
  });

  it('keeps the real deadline when the UI clock lags', () => {
    const staleUiClock = Date.parse('2026-07-13T12:00:00Z');
    expect(nextUpdateLabel('2026-07-13T12:05:20Z', staleUiClock)).toBe('Next update in 6m');
  });
});
