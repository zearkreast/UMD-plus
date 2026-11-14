import { describe, expect, it } from 'vitest';

import {
  keepSpeedHistories,
  recordSpeed,
  roundedTop,
  speedChart,
  speedHistory,
} from './speedHistory';

const MB = 1024 * 1024;

describe('the speed of a download over its last minute', () => {
  it('keeps one reading a second, and a stop within it', () => {
    recordSpeed('a', 2 * MB, 10_000);
    recordSpeed('a', 3 * MB, 10_400);
    recordSpeed('a', 1 * MB, 10_800);
    expect(speedHistory('a')).toEqual([{ at: 10_000, bps: 3 * MB }]);

    recordSpeed('a', 0, 10_900);
    expect(speedHistory('a')).toEqual([{ at: 10_000, bps: 0 }]);
  });

  it('lets go of what is older than the minute', () => {
    for (let second = 0; second <= 90; second += 1) recordSpeed('b', MB, second * 1000);
    const kept = speedHistory('b');
    expect(kept[0]!.at).toBe(29_000);
    expect(kept[kept.length - 1]!.at).toBe(90_000);
  });

  it('forgets downloads that have left the queue', () => {
    recordSpeed('gone', MB, 1000);
    recordSpeed('here', MB, 1000);
    keepSpeedHistories(new Set(['here']));
    expect(speedHistory('gone')).toEqual([]);
    expect(speedHistory('here')).toHaveLength(1);
  });
});

describe('the chart of it', () => {
  it('tops out at a figure that can be named', () => {
    expect(roundedTop(3.1 * MB)).toBe(5 * MB);
    expect(roundedTop(900 * 1024)).toBe(1000 * 1024);
    expect(roundedTop(MB)).toBe(MB);
    expect(roundedTop(0)).toBe(MB);
  });

  it('puts now at the right edge and the top speed at the top', () => {
    const samples = [
      { at: 0, bps: 0 },
      { at: 30_000, bps: 5 * MB },
      { at: 60_000, bps: 2.5 * MB },
    ];
    const chart = speedChart(samples, 60_000, 600, 100);
    expect(chart.top).toBe(5 * MB);
    expect(chart.line).toBe('M0.0,100.0 L300.0,0.0 L600.0,50.0');
    expect(chart.area.endsWith('L600.0,100.0 L0.0,100.0 Z')).toBe(true);
  });

  it('marks the stretches in which nothing arrived', () => {
    const samples = [
      { at: 0, bps: MB },
      { at: 20_000, bps: 0 },
      { at: 23_000, bps: 0 },
      { at: 26_000, bps: MB },
      { at: 60_000, bps: MB },
    ];
    expect(speedChart(samples, 60_000, 600, 100).stalls).toEqual([[200, 60]]);
  });

  it('draws nothing from a single reading', () => {
    expect(speedChart([{ at: 0, bps: MB }], 1000, 600, 100).line).toBe('');
  });
});
