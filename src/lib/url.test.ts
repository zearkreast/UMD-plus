import { describe, expect, it } from 'vitest';

import { displayLink, extractUrls, linkIn, normalizeUrl } from './url';

describe('displayLink', () => {
  it('drops the scheme, the www and a trailing slash', () => {
    expect(displayLink('https://www.instagram.com/reel/C8kLm2NoPqR/')).toBe(
      'instagram.com/reel/C8kLm2NoPqR',
    );
    expect(displayLink('https://x.com/')).toBe('x.com');
  });

  it('keeps the query, where a video id can live', () => {
    expect(displayLink('https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10s')).toBe(
      'youtube.com/watch?v=dQw4w9WgXcQ&t=10s',
    );
  });

  it('shows a stored link in its own letters', () => {
    const stored = normalizeUrl('https://www.reddit.com/r/Turkey/comments/1abc/bugün_hava_çok_güzel/')!;
    expect(stored).toContain('%C3%BC');
    expect(displayLink(stored)).toBe('reddit.com/r/Turkey/comments/1abc/bugün_hava_çok_güzel');
  });

  it('leaves what it cannot read as it was', () => {
    expect(displayLink('https://example.com/100%')).toBe('example.com/100%');
    expect(displayLink('  not a link ')).toBe('not a link');
  });
});

describe('the link in a pasted text', () => {
  it('is the first of two on two lines, not the two run together', () => {
    expect(linkIn('https://youtu.be/aaa\nhttps://youtu.be/bbb')).toBe('https://youtu.be/aaa');
    expect(linkIn('https://youtu.be/aaa\r\nhttps://www.tiktok.com/@x/video/1')).toBe(
      'https://youtu.be/aaa',
    );
  });

  it('is found among the words around it', () => {
    expect(linkIn('Şu videoya bak https://youtu.be/aaa')).toBe('https://youtu.be/aaa');
  });

  it('may be a bare address', () => {
    expect(linkIn('  youtu.be/aaa  ')).toBe('https://youtu.be/aaa');
    expect(linkIn('not a link at all')).toBeNull();
    expect(linkIn('')).toBeNull();
  });
});

describe('every link in a text', () => {
  it('is found once each, in the order they stand', () => {
    const text = 'https://youtu.be/aaa\nsonra www.tiktok.com/@x/video/1, bir de https://youtu.be/aaa.';
    expect(extractUrls(text)).toEqual(['https://youtu.be/aaa', 'https://www.tiktok.com/@x/video/1']);
  });

  it('is none where there is none', () => {
    expect(extractUrls('yalnızca yazı')).toEqual([]);
    expect(extractUrls('')).toEqual([]);
  });
});
