import { describe, expect, it } from 'vitest';

import type { DownloadTask } from '@/types';
import { groupDownloads, placesInLine, reconcileOrder } from './downloadGroups';

const task = (id: string, status: DownloadTask['status'], times: Partial<DownloadTask> = {}) =>
  ({ id, status, createdAt: 0, startedAt: null, completedAt: null, ...times }) as DownloadTask;

describe('the downloads list in its three parts', () => {
  it('keeps the waiting ones in the order the queue starts them', () => {
    const groups = groupDownloads([
      task('done', 'completed', { completedAt: 5 }),
      task('first', 'queued', { createdAt: 9 }),
      task('held', 'paused', { createdAt: 3 }),
      task('second', 'queued', { createdAt: 1 }),
      task('live', 'downloading', { startedAt: 2 }),
    ]);
    expect(groups.pending.map((entry) => entry.id)).toEqual(['first', 'held', 'second']);
    expect(groups.running.map((entry) => entry.id)).toEqual(['live']);
    expect(groups.finished.map((entry) => entry.id)).toEqual(['done']);
  });

  it('puts the latest finished on top and the first started on top', () => {
    const groups = groupDownloads([
      task('a', 'completed', { completedAt: 1 }),
      task('b', 'failed', { completedAt: 3 }),
      task('c', 'canceled', { createdAt: 2 }),
      task('late', 'processing', { startedAt: 8 }),
      task('early', 'downloading', { startedAt: 4 }),
    ]);
    expect(groups.finished.map((entry) => entry.id)).toEqual(['b', 'c', 'a']);
    expect(groups.running.map((entry) => entry.id)).toEqual(['early', 'late']);
  });

  it('numbers only what will start by itself', () => {
    const places = placesInLine([task('x', 'queued'), task('y', 'paused'), task('z', 'queued')]);
    expect([...places]).toEqual([
      ['x', 1],
      ['z', 2],
    ]);
  });
});

describe('an order being dragged', () => {
  it('follows the list it is an order of', () => {
    expect(reconcileOrder(['b', 'a', 'gone'], ['a', 'b', 'new'])).toEqual(['b', 'a', 'new']);
    expect(reconcileOrder([], ['a', 'b'])).toEqual(['a', 'b']);
  });
});
