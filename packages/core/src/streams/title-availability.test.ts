import './filterer.js';
import assert from 'node:assert/strict';
import { it } from 'node:test';
import type { Metadata } from '../metadata/utils.js';
import type { UserData } from '../db/schemas.js';
import StreamFilterer from './filterer.js';
import { titleCountryEvidence } from './title-conflicts.js';
import { SkyhookMetadata } from '../metadata/skyhook.js';
import { TMDBMetadata } from '../metadata/tmdb.js';
import { initialiseTestSettings, mockHttp } from '../../test/helpers.js';
import { release, seriesContext } from '../../test/title-fixtures.js';

const options = {
  titleMatching: { enabled: true, mode: 'exact', ambiguousResults: 'discard' },
  seasonEpisodeMatching: { enabled: true },
  languageInference: { enabled: false },
} as UserData;

const news: Metadata = {
  title: '20/20',
  titles: [{ title: '20/20' }, { title: '20.20' }, { title: 'ABC 20/20' }],
  year: 1978,
  country: 'US',
  genres: ['News', 'Documentary', 'Talk Show', 'Crime'],
  tvdbId: 72289,
  isDateBased: true,
  episodeAirDates: ['2026-10-02'],
  seasons: [{ season_number: 50, episode_count: 5 }],
  titleConflicts: [
    { title: '20/20 (NZ)', year: 2011, country: 'NZ', tvdbId: 278536 },
  ],
};

it('keeps the original daily news edition without relying on year-numbered competitor seasons', async (t) => {
  await initialiseTestSettings(t);
  mockHttp(t);
  t.mock.method(SkyhookMetadata.prototype, 'search', async () => []);
  t.mock.method(SkyhookMetadata.prototype, 'getShow', async (tvdbId) => ({
    tvdbId,
    title: '20/20 (NZ)',
    status: 'Ended',
    // Public catalogues can combine ordinal and year-based seasons.
    episodes: [
      { seasonNumber: 9, episodeNumber: 1 },
      { seasonNumber: 2011, episodeNumber: 1 },
    ],
  }));
  const names = [
    '20.20.2026.10.02.1080p.WEB.h264-GROUP.mkv',
    '20.20.S50E02.1080p.WEB.h264-GROUP.mkv',
    '20.20.US.S50E02.1080p.WEB.h264-GROUP.mkv',
    '20.20.NZ.S50E02.1080p.WEB.h264-GROUP.mkv',
    '20.20.S50E03.1080p.WEB.h264-GROUP.mkv',
    'Unrelated.Show.S50E02.1080p.WEB.h264-GROUP.mkv',
  ];
  const kept = await new StreamFilterer(options).filter(
    names.map((name) => release(name)),
    seriesContext(news, {
      id: 'tt0124932:50:2',
      parsedId: {
        type: 'imdbId',
        value: 'tt0124932',
        season: '50',
        episode: '2',
      },
    })
  );
  assert.deepEqual(
    kept.map((s) => s.filename),
    names.slice(0, 3)
  );
});

it('does not infer a daily news country from aliases, remakes, later editions or incomplete identities', () => {
  assert.equal(titleCountryEvidence(news, '20/20'), 'US');
  assert.equal(titleCountryEvidence(news, 'ABC 20/20'), undefined);
  for (const overrides of [
    { genres: ['Drama'] },
    { genres: undefined },
    { isDateBased: false },
    { country: undefined },
    { year: undefined },
    { title: '20/20 (US)' },
    { titleConflicts: [{ title: '20/20', year: 2020, country: 'US' }] },
    { titleConflicts: [{ title: '20/20 (NZ)', country: 'NZ' }] },
    { titleConflicts: [{ title: '20/20 (NZ)', year: 1970, country: 'NZ' }] },
    { titleConflicts: [{ title: '20/20', year: 2011 }] },
    {
      titleConflicts: [...news.titleConflicts!, { title: '20/20', year: 2020 }],
    },
  ])
    assert.equal(
      titleCountryEvidence({ ...news, ...overrides }, '20/20'),
      undefined
    );
  assert.equal(titleCountryEvidence(news, '20/20', 'NZ'), 'NZ');
});

const cyberpunk: Metadata = {
  title: 'Cyberpunk: Edgerunners',
  titles: [
    { title: 'Cyberpunk: Edgerunners' },
    { title: 'Cyberpunk: Edgerunners 2' },
  ],
  year: 2022,
  country: 'JP',
  tvdbId: 384541,
  genres: ['Animation'],
  seasons: [{ season_number: 2, episode_count: 10 }],
};
const cyberpunkContext = (metadata: Metadata) =>
  seriesContext(metadata, {
    id: 'tt12590266:2:5',
    isAnime: true,
    parsedId: {
      type: 'imdbId',
      value: 'tt12590266',
      season: '2',
      episode: '5',
    },
  });

it('keeps matching primary releases through discovery failure while rejecting contradictory IDs and wrong titles', async (t) => {
  await initialiseTestSettings(t);
  mockHttp(t);
  t.mock.method(SkyhookMetadata.prototype, 'getShow', async () => null);
  const filename =
    'Cyberpunk.Edgerunners.S02E05.Fades.Pass.the.Distance.2160p.NF.WEB-DL.DUAL.DDP5.1.DV.HDR.H.265-Kitsune.mkv';
  const streams = [
    release(filename, { id: 'query' }),
    release(filename, { id: 'id', idMatched: true }),
    release(filename, { id: 'contradiction', releaseIds: { tvdbId: 999999 } }),
    release('Different.Show.S02E05.1080p.mkv', { id: 'wrong-title' }),
  ];
  const kept = await new StreamFilterer(options).filter(
    streams,
    cyberpunkContext({ ...cyberpunk, titleConflictsUnavailable: true })
  );
  assert.deepEqual(
    kept.map((s) => s.id),
    ['query', 'id']
  );
});

it('keeps matching aliases after lookup failure but enforces conflicts discovered before another provider failed', async (t) => {
  await initialiseTestSettings(t);
  mockHttp(t);
  t.mock.method(SkyhookMetadata.prototype, 'search', async () => {
    throw new Error('lookup unavailable');
  });
  t.mock.method(SkyhookMetadata.prototype, 'getShow', async () => null);
  const filename = 'Cyberpunk.Edgerunners.2.S02E05.1080p.WEB.h264-GROUP.mkv';
  const kept = await new StreamFilterer(options).filter(
    [release(filename)],
    cyberpunkContext(cyberpunk)
  );
  assert.equal(kept.length, 1);
  t.mock.method(TMDBMetadata.prototype, 'searchSeries', async () => [
    {
      name: 'Cyberpunk: Edgerunners 2',
      tmdbId: 999999,
      year: 2025,
      country: 'JP',
    },
  ]);
  t.mock.method(
    TMDBMetadata.prototype,
    'getSeasonBound',
    async () => undefined
  );
  t.mock.method(TMDBMetadata.prototype, 'getEpisodeCatalog', async () => null);
  const ambiguous = await new StreamFilterer({
    ...options,
    tmdbApiKey: 'test',
  }).filter([release(filename)], cyberpunkContext(cyberpunk));
  assert.equal(
    ambiguous.length,
    0,
    'a partial discovery still supplies a real conflict'
  );
});

it('does not turn a primary lookup outage into an exemption from a known collision', async (t) => {
  await initialiseTestSettings(t);
  mockHttp(t);
  t.mock.method(SkyhookMetadata.prototype, 'getShow', async () => null);
  const kept = await new StreamFilterer(options).filter(
    [release('Cyberpunk.Edgerunners.S02E05.1080p.WEB.h264-GROUP.mkv')],
    cyberpunkContext({
      ...cyberpunk,
      titleConflictsUnavailable: true,
      titleConflicts: [
        {
          title: 'Cyberpunk: Edgerunners',
          year: 2025,
          country: 'JP',
          tvdbId: 999999,
        },
      ],
    })
  );
  assert.equal(kept.length, 0);
});
