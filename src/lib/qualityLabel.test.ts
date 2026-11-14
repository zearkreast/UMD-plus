import { afterEach, describe, expect, test, vi } from 'vitest';

import { setLanguage } from '@/i18n';
import { qualityText } from '@/lib/qualityLabel';

// Changing the language marks the page with it, and there is no page here.
vi.stubGlobal('document', { documentElement: {} });

afterEach(() => setLanguage('en'));

describe('a quality the backend named in English', () => {
  test('is written in the language of the interface', () => {
    setLanguage('tr');
    expect(qualityText('Best - MP4')).toBe('En iyi kalite - MP4');
    expect(qualityText('Auto')).toBe('Otomatik');
    expect(qualityText('Original')).toBe('Orijinal');
    expect(qualityText('Original - JPG')).toBe('Orijinal - JPG');
    expect(qualityText('Audio')).toBe('Ses');
  });

  test('a figure, or a word that only starts like one, is left alone', () => {
    setLanguage('tr');
    expect(qualityText('1080p - MP4')).toBe('1080p - MP4');
    expect(qualityText('320 kbps')).toBe('320 kbps');
    expect(qualityText('Audiobook')).toBe('Audiobook');
    expect(qualityText('')).toBe('');
  });
});
