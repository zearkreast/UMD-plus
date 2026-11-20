import type { TranslationKey } from '@/i18n';
import type { BridgeSessionState, BridgeSite, BridgeStatus, TransferRefusal } from '@/types';

/** Not translated. */
export const SITE_NAMES: Record<Exclude<BridgeSite, 'other'>, string> = {
  youtube: 'YouTube',
  tiktok: 'TikTok',
  reddit: 'Reddit',
  x: 'X',
};

export function siteStates(status: BridgeStatus): [Exclude<BridgeSite, 'other'>, BridgeSessionState][] {
  return [
    ['youtube', status.session],
    ['tiktok', status.tiktokSession],
    ['reddit', status.redditSession ?? 'none'],
    ['x', status.xSession ?? 'none'],
  ];
}

export function siteName(site: BridgeSite, status: BridgeStatus | null): string {
  if (site === 'other') return status?.otherDomain ?? 'other';
  return SITE_NAMES[site];
}

/** Fresh sessions only: a stale one would be stale on the phone too. */
export function sendableSites(status: BridgeStatus): BridgeSite[] {
  const sites: BridgeSite[] = siteStates(status)
    .filter(([, state]) => state === 'fresh')
    .map(([site]) => site);
  if (status.otherSession === 'fresh' && status.otherDomain) sites.push('other');
  return sites;
}

export const REFUSAL_KEYS: Record<TransferRefusal, TranslationKey> = {
  invalid: 'transfer.refused.invalid',
  expired: 'transfer.refused.expired',
  used: 'transfer.refused.used',
  unreachable: 'transfer.refused.unreachable',
  rejected: 'transfer.refused.rejected',
  tampered: 'transfer.refused.tampered',
  empty: 'transfer.refused.empty',
};

/** `m:ss` until `expiresAt` (epoch seconds), never below zero. */
export function countdown(expiresAt: number, nowMs: number): string {
  const left = Math.max(0, Math.ceil(expiresAt - nowMs / 1000));
  return `${Math.floor(left / 60)}:${String(left % 60).padStart(2, '0')}`;
}

export interface TransferLink {
  code: string;
  /** The computer's addresses, as the code names them, to show before asking. */
  addresses: string[];
}

/** A Send to phone code in text the phone was handed: opened from its camera, or shared. */
export function transferLinkIn(text: string): TransferLink | null {
  const code = text.match(/umdplus:\/\/transfer\?\S+/i)?.[0];
  if (!code) return null;
  try {
    const addresses = (new URL(code).searchParams.get('a') ?? '').split(',').filter(Boolean);
    return { code, addresses };
  } catch {
    return null;
  }
}
