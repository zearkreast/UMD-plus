import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/Button';
import { InlineNotice } from '@/components/ui/InlineNotice';
import { Modal } from '@/components/ui/Modal';
import { Toggle } from '@/components/ui/Toggle';
import { errorMessage, useTranslation } from '@/i18n';
import { cn } from '@/lib/cn';
import { IS_MOBILE } from '@/lib/platform';
import * as ipc from '@/services/ipc';
import type { DownloadRequest, SubscriptionBackfill } from '@/types';

interface FollowDialogProps {
  open: boolean;
  onClose: () => void;
  /** The list, with the options chosen for it on Home. */
  request: DownloadRequest | null;
  title: string;
  /** How many items the list holds now. */
  count: number;
  onFollowed: () => void;
}

/**
 * Following a list: what to do with what it already holds, and whether to
 * pass over Shorts. The rest -- video or sound, quality, format, folder -- is
 * what Home has chosen for it.
 */
export function FollowDialog({ open, onClose, request, title, count, onFollowed }: FollowDialogProps) {
  const { t } = useTranslation();
  const [backfill, setBackfill] = useState<SubscriptionBackfill>('none');
  const [skipShorts, setSkipShorts] = useState(true);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setBackfill('none');
    setSkipShorts(true);
    setFailure(null);
  }, [open]);

  const follow = async () => {
    if (!request) return;
    setBusy(true);
    setFailure(null);
    try {
      await ipc.addSubscription(request, backfill, skipShorts);
      onFollowed();
    } catch (error) {
      setFailure(errorMessage(ipc.toAppError(error), IS_MOBILE));
    } finally {
      setBusy(false);
    }
  };

  const choices: { value: SubscriptionBackfill; label: string }[] = [
    { value: 'none', label: t('follow.none') },
    { value: 'top', label: t('follow.top') },
    { value: 'all', label: t('follow.all', { n: count }) },
  ];

  return (
    <Modal
      open={open}
      onClose={busy ? () => {} : onClose}
      title={t('follow.title', { title })}
      description={t('follow.body')}
      closeLabel={t('common.close')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose} disabled={busy}>
            {t('common.cancel')}
          </Button>
          <Button variant="primary" data-autofocus loading={busy} onClick={() => void follow()}>
            {t('follow.confirm')}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3 pb-2">
        <fieldset className="flex flex-col gap-1.5">
          <legend className="mb-1.5 text-[12.5px] font-semibold text-fg-muted">
            {t('follow.existing', { n: count })}
          </legend>
          {choices.map((choice) => (
            <label
              key={choice.value}
              className={cn(
                'flex cursor-pointer items-center gap-2.5 rounded-[var(--radius-control)] px-3 py-2 text-[13px]',
                backfill === choice.value ? 'bg-accent-soft font-medium text-accent' : 'bg-fill text-fg hover:bg-fill-hover',
              )}
            >
              <input
                type="radio"
                name="follow-backfill"
                value={choice.value}
                checked={backfill === choice.value}
                onChange={() => setBackfill(choice.value)}
                className="accent-[var(--accent)]"
              />
              {choice.label}
            </label>
          ))}
        </fieldset>

        <div className="flex items-center justify-between gap-3 rounded-[var(--radius-control)] bg-fill px-3 py-2.5">
          <span className="min-w-0">
            <span className="block text-[13px] font-medium text-fg">{t('subs.skipShorts')}</span>
            <span className="block text-[12px] text-fg-muted">{t('subs.skipShortsHint')}</span>
          </span>
          <Toggle checked={skipShorts} onChange={setSkipShorts} label={t('subs.skipShorts')} />
        </div>

        {failure && <InlineNotice tone="error">{failure}</InlineNotice>}
      </div>
    </Modal>
  );
}
