import type { DownloadTask } from '@/types';

/**
 * The downloads list in the three parts it is read in: what is running, what
 * is still to run, and what is over.
 */
export interface DownloadGroups {
  /** Running, the one started first on top. */
  running: DownloadTask[];
  /**
   * Waiting for a slot or paused, in the queue's own order: the first waiting
   * one here is the next to start.
   */
  pending: DownloadTask[];
  /** Completed, failed or cancelled, the latest on top. */
  finished: DownloadTask[];
}

const RUNNING = new Set<DownloadTask['status']>(['preparing', 'downloading', 'processing']);
const PENDING = new Set<DownloadTask['status']>(['queued', 'paused']);

/** `tasks` is in the queue's order, which is the order it starts them in. */
export function groupDownloads(tasks: readonly DownloadTask[]): DownloadGroups {
  const running = tasks
    .filter((task) => RUNNING.has(task.status))
    .sort((a, b) => (a.startedAt ?? a.createdAt) - (b.startedAt ?? b.createdAt));
  const pending = tasks.filter((task) => PENDING.has(task.status));
  const finished = tasks
    .filter((task) => !RUNNING.has(task.status) && !PENDING.has(task.status))
    .sort((a, b) => (b.completedAt ?? b.createdAt) - (a.completedAt ?? a.createdAt));
  return { running, pending, finished };
}

/**
 * Each waiting download's place in line, from 1. A paused one has none: it
 * does not start until it is resumed, wherever it stands.
 */
export function placesInLine(pending: readonly DownloadTask[]): Map<string, number> {
  const places = new Map<string, number>();
  for (const task of pending) {
    if (task.status === 'queued') places.set(task.id, places.size + 1);
  }
  return places;
}

/**
 * `shown` with whatever of it is no longer in `ids` dropped and whatever is
 * new in `ids` added at the end: an order the user is dragging, kept true to
 * the list it is an order of.
 */
export function reconcileOrder(shown: readonly string[], ids: readonly string[]): string[] {
  const present = new Set(ids);
  const kept = shown.filter((id) => present.has(id));
  const known = new Set(kept);
  return [...kept, ...ids.filter((id) => !known.has(id))];
}
