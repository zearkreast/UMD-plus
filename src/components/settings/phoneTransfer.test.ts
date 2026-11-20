import { describe, expect, test } from 'vitest';

import type { BridgeStatus } from '@/types';
import { countdown, sendableSites, siteName } from './phoneTransfer';

function status(patch: Partial<BridgeStatus> = {}): BridgeStatus {
  return {
    supported: true,
    storeListed: true,
    enabled: true,
    registered: true,
    connected: true,
    browser: 'Chrome',
    profileLabel: null,
    accountHint: null,
    extensionVersion: null,
    lastPushAt: null,
    session: 'none',
    tiktokSession: 'none',
    tiktokLastPushAt: null,
    redditSession: 'none',
    xSession: 'none',
    otherSession: 'none',
    otherDomain: null,
    hostPath: null,
    appVersion: '0.0.0',
    extensionId: 'x',
    ...patch,
  };
}

describe('sendableSites', () => {
  test('offers nothing when no session is fresh', () => {
    expect(sendableSites(status({ session: 'stale', tiktokSession: 'stale' }))).toEqual([]);
  });

  test('offers every fresh session, the other site only with its domain', () => {
    expect(
      sendableSites(
        status({
          session: 'fresh',
          tiktokSession: 'stale',
          xSession: 'fresh',
          otherSession: 'fresh',
          otherDomain: 'instagram.com',
        }),
      ),
    ).toEqual(['youtube', 'x', 'other']);
    expect(sendableSites(status({ otherSession: 'fresh', otherDomain: null }))).toEqual([]);
  });
});

describe('siteName', () => {
  test('names the other-site slot by its domain', () => {
    expect(siteName('other', status({ otherDomain: 'instagram.com' }))).toBe('instagram.com');
    expect(siteName('tiktok', null)).toBe('TikTok');
  });
});

describe('countdown', () => {
  test('reads minutes and seconds, and stops at zero', () => {
    expect(countdown(1_000_120, 1_000_000_000)).toBe('2:00');
    expect(countdown(1_000_065, 1_000_000_500)).toBe('1:05');
    expect(countdown(1_000_000, 1_000_009_000)).toBe('0:00');
  });
});
