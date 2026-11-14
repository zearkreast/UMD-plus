/**
 * The speed of each running download over its last minute, for the chart in
 * its detail window. Kept beside the queue rather than in it: a reading a
 * second is not something any row needs to re-render for.
 */

export interface SpeedSample {
  /** Milliseconds since the epoch. */
  at: number;
  bps: number;
}

/** How far back the chart looks. */
export const SPEED_WINDOW_MS = 60_000;

/** Progress arrives several times a second; the chart needs one reading of it. */
const EVERY_MS = 1000;

const histories = new Map<string, SpeedSample[]>();

export function recordSpeed(id: string, bps: number, at: number): void {
  const samples = histories.get(id) ?? [];
  const last = samples[samples.length - 1];
  const value = Number.isFinite(bps) && bps > 0 ? bps : 0;
  if (last && at - last.at < EVERY_MS) {
    // Within the same second the fastest reading stands, so a burst between
    // two ticks is not lost -- except a stop, which is the news.
    last.bps = value === 0 ? 0 : Math.max(last.bps, value);
    return;
  }
  samples.push({ at, bps: value });
  while (samples.length > 0 && at - samples[0]!.at > SPEED_WINDOW_MS + EVERY_MS) samples.shift();
  histories.set(id, samples);
}

export function speedHistory(id: string): readonly SpeedSample[] {
  return histories.get(id) ?? [];
}

/** Drop what is kept for downloads that are no longer in the queue. */
export function keepSpeedHistories(ids: ReadonlySet<string>): void {
  for (const id of histories.keys()) {
    if (!ids.has(id)) histories.delete(id);
  }
}

export interface SpeedChart {
  /** The line, as an SVG path over `width` by `height`. Empty with under two readings. */
  line: string;
  /** The same shape closed down to the baseline, for the fill under it. */
  area: string;
  /** The speed the top of the chart stands for. */
  top: number;
  /** Stretches where nothing arrived, as `[x, width]` in the chart's units. */
  stalls: [number, number][];
}

/** A round figure at or above `bps`, in steps a reader can name: 1, 2 or 5 of a unit. */
export function roundedTop(bps: number): number {
  if (!(bps > 0)) return 1024 * 1024;
  let unit = 1;
  while (unit * 1024 <= bps) unit *= 1024;
  for (const step of [1, 2, 5, 10, 20, 50, 100, 200, 500, 1000]) {
    if (step * unit >= bps) return step * unit;
  }
  return 1024 * unit;
}

/**
 * The last minute up to `now`, drawn left to right with `now` at the right
 * edge. A reading stands until the next one, so the line is not drawn through
 * time in which nothing was reported.
 */
export function speedChart(
  samples: readonly SpeedSample[],
  now: number,
  width: number,
  height: number,
): SpeedChart {
  const shown = samples.filter((sample) => now - sample.at <= SPEED_WINDOW_MS);
  const top = roundedTop(Math.max(0, ...shown.map((sample) => sample.bps)));
  if (shown.length < 2) return { line: '', area: '', top, stalls: [] };

  const x = (at: number) => Math.max(0, width - ((now - at) / SPEED_WINDOW_MS) * width);
  const y = (bps: number) => height - (Math.min(bps, top) / top) * height;
  const fixed = (value: number) => value.toFixed(1);

  const points = shown.map((sample) => `${fixed(x(sample.at))},${fixed(y(sample.bps))}`);
  const line = `M${points.join(' L')}`;
  const first = fixed(x(shown[0]!.at));
  const last = fixed(x(shown[shown.length - 1]!.at));
  const area = `${line} L${last},${fixed(height)} L${first},${fixed(height)} Z`;

  const stalls: [number, number][] = [];
  let from: number | null = null;
  for (const sample of shown) {
    if (sample.bps === 0) {
      from ??= sample.at;
    } else if (from != null) {
      stalls.push([x(from), x(sample.at) - x(from)]);
      from = null;
    }
  }
  if (from != null) stalls.push([x(from), x(shown[shown.length - 1]!.at) - x(from)]);

  return { line, area, top, stalls: stalls.filter(([, wide]) => wide > 0) };
}
