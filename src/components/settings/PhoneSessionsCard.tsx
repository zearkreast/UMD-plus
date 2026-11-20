import {
  cancel as cancelScan,
  checkPermissions,
  Format,
  openAppSettings,
  requestPermissions,
  scan,
} from '@tauri-apps/plugin-barcode-scanner';
import { QrCode } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/Button';
import { InlineNotice } from '@/components/ui/InlineNotice';
import { SettingGroup } from '@/components/ui/SettingRow';
import { useBackLayer } from '@/hooks/useBackLayer';
import { useTranslation } from '@/i18n';
import type { TranslationKey } from '@/i18n';
import { cn } from '@/lib/cn';
import * as ipc from '@/services/ipc';
import type { BridgeSite, BridgeStatus } from '@/types';
import {
  BODY_SIZE,
  Disclosure,
  SITE_STATE,
  SiteRow,
  TITLE_SIZE,
  useBridgeStatus,
} from './BrowserLinkCard';
import { REFUSAL_KEYS, SITE_NAMES, siteName, siteStates } from './phoneTransfer';

type Result =
  | { kind: 'received'; sites: BridgeSite[] }
  | { kind: 'error'; message: TranslationKey }
  | { kind: 'camera' };

interface PhoneSessionsCardProps {
  initialStatus?: BridgeStatus | null;
}

export function PhoneSessionsCard({ initialStatus = null }: PhoneSessionsCardProps) {
  const { t } = useTranslation();
  const status = useBridgeStatus(0, initialStatus);
  const [scanning, setScanning] = useState(false);
  const [receiving, setReceiving] = useState(false);
  const [result, setResult] = useState<Result | null>(null);
  const [forgetFailed, setForgetFailed] = useState(false);

  // The camera preview covers the page and has no cancel button.
  useBackLayer(scanning, () => void cancelScan().catch(() => {}));

  const take = async () => {
    setResult(null);
    try {
      let permission = await checkPermissions();
      if (permission !== 'granted') permission = await requestPermissions();
      if (permission !== 'granted') {
        setResult({ kind: 'camera' });
        return;
      }
    } catch {
      setResult({ kind: 'camera' });
      return;
    }

    let code: string;
    setScanning(true);
    try {
      code = (await scan({ windowed: false, formats: [Format.QRCode] })).content;
    } catch {
      // Cancelled with Back.
      return;
    } finally {
      setScanning(false);
    }

    setReceiving(true);
    try {
      const receipt = await ipc.phoneTransferReceive(code);
      setResult(
        receipt.outcome === 'received'
          ? { kind: 'received', sites: receipt.sites }
          : { kind: 'error', message: REFUSAL_KEYS[receipt.reason] },
      );
    } catch {
      setResult({ kind: 'error', message: 'transfer.receiveFailed' });
    } finally {
      setReceiving(false);
    }
  };

  const forget = async (site: BridgeSite) => {
    setForgetFailed(false);
    try {
      await ipc.bridgeForgetSession(site);
    } catch {
      setForgetFailed(true);
    }
  };

  const states = status ? siteStates(status) : [];
  const other = status?.otherSession === 'fresh' ? status.otherDomain : null;

  return (
    <SettingGroup>
      <div className="px-4 py-4">
        <p className={cn('text-fg', TITLE_SIZE)}>{t('transfer.phoneTitle')}</p>
        <p className={cn('mt-1 leading-relaxed text-fg-muted', BODY_SIZE)}>
          {t('transfer.phoneHint')}
        </p>

        <Button
          className="mt-3"
          variant="primary"
          fullWidth
          icon={<QrCode size={16} />}
          loading={receiving}
          disabled={scanning}
          onClick={() => void take()}
        >
          {receiving ? t('transfer.receiving') : t('transfer.scan')}
        </Button>

        {result?.kind === 'received' && (
          <InlineNotice tone="success" className="mt-2.5">
            {t('transfer.received', {
              sites: result.sites.map((site) => siteName(site, status)).join(', '),
            })}
          </InlineNotice>
        )}
        {result?.kind === 'error' && (
          <InlineNotice tone="error" className="mt-2.5">
            {t(result.message)}
          </InlineNotice>
        )}
        {result?.kind === 'camera' && (
          <div className="mt-2.5">
            <InlineNotice tone="error">{t('transfer.cameraDenied')}</InlineNotice>
            <Button
              size="sm"
              variant="ghost"
              className="-ml-3 mt-1"
              onClick={() => void openAppSettings().catch(() => {})}
            >
              {t('transfer.openSettings')}
            </Button>
          </div>
        )}

        {status && (
          <dl
            className={cn(
              'mt-4 grid grid-cols-[1fr_auto] items-center gap-x-3 gap-y-2',
              BODY_SIZE,
            )}
          >
            {states.map(([site, state]) => (
              <SiteRow
                key={site}
                name={SITE_NAMES[site]}
                fresh={state === 'fresh'}
                label={t(SITE_STATE[state])}
                onForget={state === 'none' ? null : () => void forget(site)}
              />
            ))}
            <SiteRow
              name={t('settings.linkOtherSites')}
              fresh={other != null}
              label={other ? t('settings.linkOtherFresh', { site: other }) : t('settings.linkSiteNone')}
              onForget={other ? () => void forget('other') : null}
            />
          </dl>
        )}
        {states.some(([, state]) => state === 'stale') && (
          <p className={cn('mt-2 leading-relaxed text-fg-muted', BODY_SIZE)}>
            {t('transfer.staleHint')}
          </p>
        )}
        {forgetFailed && (
          <InlineNotice tone="error" className="mt-2">
            {t('settings.linkForgetFailed')}
          </InlineNotice>
        )}
      </div>

      <Disclosure title={t('transfer.howTitle')}>
        <p>{t('transfer.howHint')}</p>
      </Disclosure>
      <Disclosure title={t('transfer.networkTitle')}>
        <p>{t('transfer.networkHint')}</p>
      </Disclosure>
    </SettingGroup>
  );
}
