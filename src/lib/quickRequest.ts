import type { HandoffContext } from '@/lib/handoff';
import type { DownloadMode, DownloadRequest, QualityPreference } from '@/types';

/**
 * A download of `url` as it is asked for without opening it on Home first:
 * nothing picked by hand, so the queue reads the link itself and takes what
 * these options name. A link the browser handed over keeps the page and
 * headers it played under, without which its server may not answer.
 */
export function quickRequest(
  url: string,
  options: { mode: DownloadMode; quality: QualityPreference; container: string | null },
  context?: HandoffContext | null,
): DownloadRequest {
  return {
    url,
    mode: options.mode,
    quality: options.quality,
    videoFormatId: null,
    audioFormatId: null,
    container: options.container,
    watermark: 'any',
    outputDir: null,
    title: context?.title ?? null,
    thumbnailUrl: context?.thumbnail ?? null,
    platform: null,
    source: context?.source ?? null,
  };
}
