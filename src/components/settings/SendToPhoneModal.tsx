import { CircleAlert, CircleCheck, Square, SquareCheck } from 'lucide-react';
import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/Button';
import { InlineNotice } from '@/components/ui/InlineNotice';
import { ListGroup } from '@/components/ui/ListGroup';
import { Modal } from '@/components/ui/Modal';
import { Spinner } from '@/components/ui/Spinner';
import { useTranslation } from '@/i18n';
import type { TranslationKey } from '@/i18n';
import { cn } from '@/lib/cn';
import * as ipc from '@/services/ipc';
import type { BridgeSite, BridgeStatus, TransferPhase, TransferStart } from '@/types';
import { countdown, sendableSites, siteName } from './phoneTransfer';

type Ready = Extract<TransferStart, { outcome: 'ready' }>;

type Step =
  | { kind: 'choose' }
  | { kind: 'code'; offer: Ready; phase: TransferPhase }
  | { kind: 'noNetwork' }
  | { kind: 'nothingToSend' }
  | { kind: 'startFailed' };

const ENDED: Partial<Record<TransferPhase, { title: TranslationKey; body: TranslationKey }>> = {
  expired: { title: 'transfer.expired', body: 'transfer.expiredHint' },
  failed: { title: 'transfer.failed', body: 'transfer.failedHint' },
};

interface SendToPhoneModalProps {
  open: boolean;
  onClose: () => void;
  status: BridgeStatus;
}

/**
 * The choice of sessions is the confirmation: a scan is answered without a
 * second question, since only someone looking at this screen can read it.
 */
export function SendToPhoneModal({ open, onClose, status }: SendToPhoneModalProps) {
  const { t } = useTranslation();
  const offered = sendableSites(status);
  const [chosen, setChosen] = useState<BridgeSite[]>(offered);
  const [step, setStep] = useState<Step>({ kind: 'choose' });
  const [starting, setStarting] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  // Closing the dialog closes any listener it left open.
  useEffect(() => {
    if (!open) return;
    setChosen(sendableSites(status));
    setStep({ kind: 'choose' });
    return () => {
      void ipc.phoneTransferCancel().catch(() => {});
    };
    // Not on `status`: the poll must not undo the choice.
  }, [open]);

  const listening = step.kind === 'code' && (step.phase === 'waiting' || step.phase === 'sending');

  useEffect(() => {
    if (!listening) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    const unlisten = ipc.onTransferChanged(() => {
      void ipc
        .phoneTransferStatus()
        .then((phase) => {
          if (!phase) return;
          setStep((current) => (current.kind === 'code' ? { ...current, phase } : current));
        })
        .catch(() => {});
    });
    return () => {
      window.clearInterval(timer);
      void unlisten.then((off) => off());
    };
  }, [listening]);

  const show = async () => {
    setStarting(true);
    try {
      const started = await ipc.phoneTransferStart(chosen);
      setNow(Date.now());
      if (started.outcome === 'ready') setStep({ kind: 'code', offer: started, phase: 'waiting' });
      else setStep({ kind: started.outcome });
    } catch {
      setStep({ kind: 'startFailed' });
    } finally {
      setStarting(false);
    }
  };

  const toggle = (site: BridgeSite) =>
    setChosen((current) =>
      current.includes(site) ? current.filter((kept) => kept !== site) : [...current, site],
    );

  // The backend's expiry event may lag the local clock by a moment.
  const phase: TransferPhase | null =
    step.kind === 'code'
      ? step.phase === 'waiting' && step.offer.expiresAt * 1000 <= now
        ? 'expired'
        : step.phase
      : null;
  const ended = phase ? ENDED[phase] : undefined;

  let footer;
  if (step.kind === 'choose') {
    footer = (
      <>
        <Button variant="ghost" onClick={onClose}>
          {t('common.cancel')}
        </Button>
        <Button
          variant="primary"
          loading={starting}
          disabled={chosen.length === 0}
          onClick={() => void show()}
        >
          {t('transfer.showCode')}
        </Button>
      </>
    );
  } else if (phase === 'sent' || step.kind === 'nothingToSend') {
    footer = (
      <Button variant="primary" onClick={onClose}>
        {t('common.close')}
      </Button>
    );
  } else if (phase === 'waiting' || phase === 'sending') {
    footer = (
      <Button variant="ghost" onClick={onClose}>
        {t('common.cancel')}
      </Button>
    );
  } else {
    footer = (
      <>
        <Button variant="ghost" onClick={onClose}>
          {t('common.close')}
        </Button>
        <Button variant="primary" loading={starting} onClick={() => void show()}>
          {t('transfer.tryAgain')}
        </Button>
      </>
    );
  }

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t('transfer.sendTitle')}
      description={step.kind === 'choose' ? t('transfer.chooseHint') : undefined}
      closeLabel={t('common.close')}
      width={420}
      footer={footer}
    >
      <div className="pb-2">
        {step.kind === 'choose' && (
          <>
            <ListGroup tone="fill">
              {offered.map((site) => {
                const on = chosen.includes(site);
                const Mark = on ? SquareCheck : Square;
                return (
                  <button
                    key={site}
                    type="button"
                    role="checkbox"
                    aria-checked={on}
                    onClick={() => toggle(site)}
                    className="flex min-h-10 w-full items-center gap-3 px-4 py-2 text-left text-[13.5px] transition-colors hover:bg-fill active:bg-fill-hover"
                  >
                    <Mark
                      size={17}
                      aria-hidden="true"
                      className={cn('shrink-0', on ? 'text-accent' : 'text-fg-faint')}
                    />
                    <span className="min-w-0 flex-1 truncate text-fg">{siteName(site, status)}</span>
                  </button>
                );
              })}
            </ListGroup>
            <p className="mt-3 text-[12.5px] leading-relaxed text-fg-muted">
              {t('transfer.sameNetwork')}
            </p>
          </>
        )}

        {step.kind === 'code' && (phase === 'waiting' || phase === 'sending') && (
          <div className="flex flex-col items-center text-center">
            <div className="relative rounded-[14px] bg-white p-2 shadow-sm">
              <img
                src={step.offer.qr}
                alt={t('transfer.codeAlt')}
                width={232}
                height={232}
                className={cn('block transition-opacity', phase === 'sending' && 'opacity-20')}
              />
              {phase === 'sending' && (
                <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 text-[13px] font-medium text-neutral-900">
                  <Spinner size={22} />
                  {t('transfer.sending')}
                </div>
              )}
            </div>
            <p className="tabular mt-3 text-[13px] font-medium text-fg">
              {t('transfer.expiresIn', { time: countdown(step.offer.expiresAt, now) })}
            </p>
            <p className="mt-1.5 text-[12.5px] leading-relaxed text-fg-muted">
              {t('transfer.scanHint')}
            </p>
            <p className="mt-2 text-[12px] leading-relaxed text-fg-faint">
              {t('transfer.addresses', { addresses: step.offer.addresses.join(', ') })}
              {' · '}
              {t('transfer.firewallHint')}
            </p>
          </div>
        )}

        {phase === 'sent' && (
          <Outcome icon="success" title={t('transfer.sent')} body={t('transfer.sentHint')} />
        )}
        {ended && <Outcome icon="error" title={t(ended.title)} body={t(ended.body)} />}
        {step.kind === 'noNetwork' && (
          <Outcome icon="error" title={t('transfer.noNetwork')} body={t('transfer.noNetworkHint')} />
        )}
        {step.kind === 'nothingToSend' && (
          <InlineNotice tone="info">{t('transfer.nothingToSend')}</InlineNotice>
        )}
        {step.kind === 'startFailed' && (
          <InlineNotice tone="error">{t('transfer.startFailed')}</InlineNotice>
        )}
      </div>
    </Modal>
  );
}

function Outcome({ icon, title, body }: { icon: 'success' | 'error'; title: string; body: string }) {
  const Icon = icon === 'success' ? CircleCheck : CircleAlert;
  return (
    <div className="flex items-start gap-2.5 py-1" role={icon === 'error' ? 'alert' : 'status'}>
      <Icon
        size={18}
        aria-hidden="true"
        className={cn('mt-[1px] shrink-0', icon === 'success' ? 'text-success' : 'text-warning')}
      />
      <div className="min-w-0">
        <p className="text-[14px] font-medium text-fg">{title}</p>
        <p className="mt-0.5 text-[12.5px] leading-relaxed text-fg-muted">{body}</p>
      </div>
    </div>
  );
}
