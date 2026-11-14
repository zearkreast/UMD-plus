import { Download, X } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/Button';
import { IconButton } from '@/components/ui/IconButton';
import { InlineNotice } from '@/components/ui/InlineNotice';
import { ListGroup } from '@/components/ui/ListGroup';
import { errorMessage, useTranslation } from '@/i18n';
import { IS_MOBILE } from '@/lib/platform';
import { quickRequest } from '@/lib/quickRequest';
import { displayLink } from '@/lib/url';
import * as ipc from '@/services/ipc';
import type { Settings } from '@/types';

interface BatchLinksProps {
  links: string[];
  settings: Settings;
  /** The list with one link taken out, or null once none are left. */
  onChange: (links: string[] | null) => void;
  /** Every link is in the queue. */
  onQueued: () => void;
}

/**
 * Several links pasted at once. Home opens one link at a time, to choose what
 * to take of it; a handful pasted together are queued as they are instead,
 * each with the default options, which is what pasting them one by one and
 * pressing Download each time would have chosen.
 */
export function BatchLinks({ links, settings, onChange, onQueued }: BatchLinksProps) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  const queueAll = async () => {
    setBusy(true);
    setFailure(null);
    const options = {
      mode: settings.defaultMode,
      quality: settings.defaultQuality,
      container: settings.defaultContainer,
    };
    // One at a time, so they stand in the queue in the order they were pasted.
    const left: string[] = [];
    let reason: string | null = null;
    for (const link of links) {
      try {
        await ipc.enqueueDownload(quickRequest(link, options));
      } catch (error) {
        left.push(link);
        reason ??= errorMessage(ipc.toAppError(error), IS_MOBILE);
      }
    }
    setBusy(false);
    if (left.length === 0) {
      onQueued();
      return;
    }
    // What would not queue stays, to be tried again or taken out.
    setFailure(reason);
    onChange(left);
  };

  return (
    <div className="flex flex-col gap-3">
      <ListGroup>
        {links.map((link) => (
          <div key={link} className="flex min-w-0 items-center gap-2 py-1.5 pl-4 pr-2">
            <span className="min-w-0 flex-1 truncate text-[13px] text-fg">{displayLink(link)}</span>
            <IconButton
              icon={<X size={15} />}
              label={t('batch.remove')}
              size="sm"
              disabled={busy}
              onClick={() => {
                const rest = links.filter((entry) => entry !== link);
                onChange(rest.length > 0 ? rest : null);
              }}
            />
          </div>
        ))}
      </ListGroup>

      {failure && <InlineNotice tone="error">{failure}</InlineNotice>}

      <p className="px-1 text-[12.5px] leading-relaxed text-fg-muted">{t('batch.hint')}</p>

      <div className="flex gap-2">
        <Button variant="ghost" disabled={busy} onClick={() => onChange(null)}>
          {t('common.cancel')}
        </Button>
        <Button
          variant="primary"
          className="flex-1"
          loading={busy}
          icon={<Download size={15} />}
          onClick={() => void queueAll()}
        >
          {t('batch.queue', { n: links.length })}
        </Button>
      </div>
    </div>
  );
}
