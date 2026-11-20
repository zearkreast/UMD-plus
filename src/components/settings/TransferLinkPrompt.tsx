import { useState } from 'react';

import { Button } from '@/components/ui/Button';
import { InlineNotice } from '@/components/ui/InlineNotice';
import { Modal } from '@/components/ui/Modal';
import { useTranslation, type TranslationKey } from '@/i18n';
import * as ipc from '@/services/ipc';
import type { BridgeSite } from '@/types';
import { REFUSAL_KEYS, siteName, type TransferLink } from './phoneTransfer';

type State =
  | { kind: 'asking' }
  | { kind: 'receiving' }
  | { kind: 'received'; sites: BridgeSite[] }
  | { kind: 'error'; message: TranslationKey };

/**
 * A Send to phone code opened from outside the app. Any page or message can
 * link one, and following it would put someone else's sessions on the phone,
 * so nothing connects until the user says so.
 */
export function TransferLinkPrompt({ link, onClose }: { link: TransferLink; onClose: () => void }) {
  const { t } = useTranslation();
  const [state, setState] = useState<State>({ kind: 'asking' });

  const receive = async () => {
    setState({ kind: 'receiving' });
    try {
      const receipt = await ipc.phoneTransferReceive(link.code);
      setState(
        receipt.outcome === 'received'
          ? { kind: 'received', sites: receipt.sites }
          : { kind: 'error', message: REFUSAL_KEYS[receipt.reason] },
      );
    } catch {
      setState({ kind: 'error', message: 'transfer.receiveFailed' });
    }
  };

  const done = state.kind === 'received' || state.kind === 'error';

  return (
    <Modal
      open
      onClose={state.kind === 'receiving' ? () => {} : onClose}
      title={t('transfer.linkTitle')}
      description={t('transfer.linkBody', { address: link.addresses.join(', ') || '?' })}
      closeLabel={t('common.close')}
      footer={
        done ? (
          <Button variant="secondary" onClick={onClose}>
            {t('common.close')}
          </Button>
        ) : (
          <>
            <Button variant="ghost" disabled={state.kind === 'receiving'} onClick={onClose}>
              {t('common.cancel')}
            </Button>
            <Button variant="primary" loading={state.kind === 'receiving'} onClick={() => void receive()}>
              {t('transfer.linkConfirm')}
            </Button>
          </>
        )
      }
    >
      {state.kind === 'asking' && <InlineNotice>{t('transfer.linkWarning')}</InlineNotice>}
      {state.kind === 'received' && (
        <InlineNotice tone="success">
          {t('transfer.received', {
            sites: state.sites
              .map((site) => (site === 'other' ? t('settings.linkOtherSites') : siteName(site, null)))
              .join(', '),
          })}
        </InlineNotice>
      )}
      {state.kind === 'error' && <InlineNotice tone="error">{t(state.message)}</InlineNotice>}
    </Modal>
  );
}
