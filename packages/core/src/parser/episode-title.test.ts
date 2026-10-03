import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  withoutSeriesSubtitle,
  isEpisodeTitleLanguageTag,
} from './episode-title.js';

const titles = [{ title: 'BLEACH: Thousand-Year Blood War - The Calamity' }];

describe('language labels misparsed as episode names', () => {
  it('recognises complete labels in multiple languages and formats', () => {
    for (const tag of [
      'SUBFRENCH',
      'subeng',
      'Sub.Spanish',
      'Subbed German',
      'French Dub',
      'English Subtitles',
      'dub-kor',
      'vostfr',
      'SUB.Japanese',
    ])
      assert.equal(isEpisodeTitleLanguageTag(tag), true, tag);
  });
  it('preserves meaningful names, embedded labels and unknown languages', () => {
    for (const title of [
      'French',
      'Sub Zero',
      'Dub Trouble',
      'SUBFRENCH Returns',
      'English Subtitles Are Missing',
      'Pilot',
      'Original',
      'Sub Atlantis',
      'Episode 10',
    ])
      assert.equal(isEpisodeTitleLanguageTag(title), false, title);
  });
});

describe('series subtitles parsed as episode-title prefixes', () => {
  it('recognises a complete subtitle component from a cour-specific alias', () => {
    assert.equal(
      withoutSeriesSubtitle(
        'Thousand-Year Blood War SON OF DARKNESS',
        'Bleach',
        titles
      ),
      'SON OF DARKNESS'
    );
  });
  it('recognises full aliases and punctuation variations', () => {
    assert.equal(
      withoutSeriesSubtitle(
        'Thousand.Year.Blood.War.SON.OF.DARKNESS',
        'Bleach',
        [{ title: 'Bleach: Thousand-Year Blood War' }]
      ),
      'SON.OF.DARKNESS'
    );
  });
  it('keeps the complete wrong episode name for subsequent matching', () => {
    assert.equal(
      withoutSeriesSubtitle(
        'Thousand-Year Blood War THE FIRE',
        'Bleach',
        titles
      ),
      'THE FIRE'
    );
  });
  it('leaves uncorroborated, partial and embedded prefixes untouched', () => {
    for (const candidate of [
      'Unknown SON OF DARKNESS',
      'Thousand-Year SON OF DARKNESS',
      'Thousand-Year Blood Warrior SON OF DARKNESS',
      'SON OF DARKNESS Thousand-Year Blood War',
      'Thousand-Year Blood War',
    ]) {
      assert.equal(
        withoutSeriesSubtitle(candidate, 'Bleach', titles),
        candidate
      );
    }
  });
  it('requires both a parsed show name and corroborating show metadata', () => {
    const candidate = 'Thousand-Year Blood War SON OF DARKNESS';
    for (const show of [undefined, '', 'Other Show']) {
      assert.equal(withoutSeriesSubtitle(candidate, show, titles), candidate);
    }
    assert.equal(withoutSeriesSubtitle(candidate, 'Bleach', []), candidate);
  });
});
