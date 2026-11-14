import { translate, type TranslationKey } from '@/i18n';

/**
 * The backend names a quality in English where the source gave it no figure:
 * "Best", "Auto", "Original", "Audio", "Video", alone or ahead of a container
 * ("Best - MP4"). Those words are also what the history has stored, so they
 * stay as they are there and are put into the interface's language here.
 */
const WORDS: Record<string, TranslationKey> = {
  Best: 'options.qualityBest',
  Auto: 'options.qualityAuto',
  Original: 'options.audioOriginal',
  Audio: 'convert.kind.audio',
  Video: 'convert.kind.video',
};

const NAMED = /^(Best|Auto|Original|Audio|Video)(?= - |$)/;

/** A quality or format label as the interface writes it. */
export function qualityText(label: string): string {
  const word = NAMED.exec(label)?.[1];
  return word ? translate(WORDS[word]!) + label.slice(word.length) : label;
}
