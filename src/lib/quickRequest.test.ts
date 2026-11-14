import { describe, expect, it } from 'vitest';

import { homeFromHandoff } from './handoff';
import { quickRequest } from './quickRequest';
import type { Handoff } from '@/types';

const defaults = {
  defaultMode: 'video' as const,
  defaultQuality: { type: 'best' as const },
  defaultContainer: 'mp4',
};

describe('a download asked for without opening it on Home', () => {
  it('names nothing by hand', () => {
    const request = quickRequest('https://youtu.be/aaa', {
      mode: 'video',
      quality: { type: 'best' },
      container: 'mp4',
    });
    expect(request).toMatchObject({
      url: 'https://youtu.be/aaa',
      mode: 'video',
      container: 'mp4',
      videoFormatId: null,
      audioFormatId: null,
      outputDir: null,
      title: null,
      source: null,
    });
  });

  it('keeps the page and headers a handed-over link played under', () => {
    const handoff = {
      url: 'https://cdn.example/hls/master.m3u8',
      kind: 'stream',
      pageUrl: 'https://example.com/watch/1',
      referer: 'https://example.com/',
      origin: 'https://example.com',
      userAgent: 'Mozilla/5.0',
      title: ' A lecture ',
      thumbnail: 'https://example.com/t.jpg',
    } as unknown as Handoff;
    const { url, options, context } = homeFromHandoff(handoff, defaults);
    const request = quickRequest(url, options, context);
    expect(request.title).toBe('A lecture');
    expect(request.thumbnailUrl).toBe('https://example.com/t.jpg');
    expect(request.source).toMatchObject({
      pageUrl: 'https://example.com/watch/1',
      referer: 'https://example.com/',
    });
  });
});
