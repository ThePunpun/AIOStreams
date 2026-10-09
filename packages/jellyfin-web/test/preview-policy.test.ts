import { chapterLabel, parseChapters } from '../src/lib/playback/chapters.ts';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  previewMode,
  closestPreview,
  validPreviewStep,
  previewSource,
} from '../src/lib/playback/preview-policy.ts';

test('cached debrid fills; Usenet is demand-only; unsupported sources stay off', () => {
  const source = (type: string, cached = true) =>
    ({
      aiostreams: { type, cached },
      Path: 'https://example.test/video.mkv',
    }) as Parameters<typeof previewMode>[0];
  assert.equal(previewMode(source('debrid')), 'full');
  assert.equal(previewMode(source('usenet')), 'demand-only');
  assert.equal(previewMode(source('usenet', false)), 'demand-only');
  for (const type of ['p2p', 'http', 'live'])
    assert.equal(previewMode(source(type)), 'off');
  assert.equal(previewMode(source('debrid', false)), 'off');
  assert.equal(previewMode({ Path: 'https://example.test/video.mkv' }), 'off');
  assert.equal(
    previewMode({ ...source('debrid'), IsInfiniteStream: true }),
    'off'
  );
  assert.equal(
    previewMode({ ...source('usenet'), SupportsDirectPlay: false }),
    'off'
  );
});

test('nearest image respects sampled time and refuses distant scenes', () => {
  const images = new Map([
    [0, { bucket: 0, position: 0, covers_until: 2.5, image: 'first' }],
    [30, { bucket: 30, position: 58, covers_until: 62.5, image: 'later' }],
  ]);
  assert.equal(closestPreview(images, 60_000)?.image, 'later');
  assert.equal(closestPreview(images, 35_000), null);
  assert.equal(closestPreview(new Map(), 1_000), null);
});

test('long GOP coverage reaches the requested seek with half-step tolerance', () => {
  const images = new Map([
    [2, { bucket: 2, position: 7, covers_until: 25, image: 'keyframe' }],
  ]);
  assert.equal(closestPreview(images, 29_900, 10000)?.image, 'keyframe');
  assert.equal(closestPreview(images, 30_100, 10000), null);
  assert.equal(closestPreview(images, 1_900, 10000), null);
  assert.equal(closestPreview(images, 27_400, 5000)?.image, 'keyframe');
  assert.equal(closestPreview(images, 27_600, 5000), null);
  assert.equal(closestPreview(images, 29_000, 8000)?.image, 'keyframe');
  assert.equal(closestPreview(images, 29_001, 8000), null);
});

test('invalid timestamps do not produce cached previews', () => {
  const images = new Map([
    [0, { bucket: 0, position: 0, covers_until: NaN, image: 'bad' }],
  ]);
  assert.equal(closestPreview(images, 0), null);
  assert.equal(closestPreview(images, NaN), null);
  assert.equal(closestPreview(new Map(), 0, 0), null);
});

test('dynamic native spacing accepts millisecond precision within the 24-hour grid', () => {
  for (const ms of [5000, 6001, 15000, 57049, 60000, 360000])
    assert.equal(validPreviewStep(ms), true);
  for (const ms of [NaN, Infinity, 0, 4999, 7500.5, 360001])
    assert.equal(validPreviewStep(ms), false);
});

test('coarse grids cover their edges without serving unrelated distant slots', () => {
  const images = new Map([
    [
      0,
      {
        bucket: 0,
        position: 30,
        covers_from: 30,
        covers_until: 30,
        image: 'minute',
      },
    ],
  ]);
  assert.equal(closestPreview(images, 0, 60000)?.image, 'minute');
  assert.equal(closestPreview(images, 60000, 60000)?.image, 'minute');
  assert.equal(closestPreview(images, 60001, 60000), null);
});

test('a forward keyframe landing still covers the original hovered bucket', () => {
  const images = new Map([
    [
      2,
      {
        bucket: 2,
        position: 14,
        covers_from: 12.5,
        covers_until: 14,
        image: 'forward',
      },
    ],
  ]);
  assert.equal(closestPreview(images, 10_100, 5000)?.image, 'forward');
  assert.equal(closestPreview(images, 16_600, 5000), null);
  assert.equal(closestPreview(images, 9_900, 5000), null);
});

test('fractional-second grids have no floating-point gaps at shared slot edges', () => {
  const stepMs = 57049;
  const images = new Map(
    Array.from({ length: 240 }, (_, bucket) => {
      const position = (bucket + 0.5) * (stepMs / 1000);
      return [
        bucket,
        { bucket, position, covers_until: position, image: 'grid' },
      ] as const;
    })
  );
  for (let bucket = 0; bucket < 240; bucket++) {
    assert.equal(
      closestPreview(images, bucket * stepMs, stepMs)?.image,
      'grid'
    );
    assert.equal(
      closestPreview(images, (bucket + 1) * stepMs, stepMs)?.image,
      'grid'
    );
  }
  assert.equal(closestPreview(images, 240 * stepMs + 1, stepMs), null);
});

test('source colour hints distinguish reshaping from compatible Dolby Vision bases', () => {
  const source = (range: string, profile?: number) =>
    ({
      MediaStreams: [
        {
          Type: 'Video',
          Index: 0,
          VideoRangeType: range,
          DvProfile: profile,
          BitRate: 8_000_000,
        },
      ],
    }) as Parameters<typeof previewSource>[0];
  for (const [range, expected] of [
    ['SDR', 'sdr'],
    ['HDR10', 'pq'],
    ['HDR10Plus', 'pq'],
    ['HLG', 'hlg'],
    ['DOVI', 'dv'],
    ['DOVIWithHDR10', 'dv-pq'],
    ['DOVIWithHDR10Plus', 'dv-pq'],
    ['DOVIWithEL', 'dv-pq'],
    ['DOVIWithELHDR10Plus', 'dv-pq'],
    ['DOVIWithHLG', 'dv-hlg'],
    ['DOVIWithSDR', 'dv-sdr'],
    ['unrecognised', 'unknown'],
  ])
    assert.equal(previewSource(source(range)).colour, expected);
  assert.deepEqual(previewSource(source('DOVI', 5)), {
    mode: 'off',
    kind: 'unknown',
    colour: 'dv',
    dv_profile: 5,
    bitrate: 8_000_000,
  });
  const ambiguous = {
    MediaStreams: [
      ...source('SDR').MediaStreams!,
      ...source('DOVI').MediaStreams!,
    ],
  };
  assert.equal(previewSource(ambiguous).colour, 'unknown');
  assert.equal(previewSource({}).bitrate, null);
  assert.equal(previewSource(source('SDR', -1)).dv_profile, null);
});

test('Usenet mode and kind reach the native worker together', () => {
  assert.deepEqual(
    previewSource({ aiostreams: { type: 'usenet' } } as Parameters<
      typeof previewSource
    >[0]),
    {
      mode: 'demand-only',
      kind: 'usenet',
      colour: 'unknown',
      dv_profile: null,
      bitrate: null,
    }
  );
});

test('chapter labels use real names, number unnamed chapters, and stay absent without chapters', () => {
  const chapters = parseChapters([
    { time: 0, title: '  Opening  ' },
    { time: 60 },
    {
      time: 120,
      title:
        'A very long chapter name that must stay on one line in the tooltip',
    },
  ]);
  assert.equal(chapterLabel(chapters, 59_999), 'Opening');
  assert.equal(chapterLabel(chapters, 60_000), 'Chapter 2');
  assert.equal(chapterLabel(chapters, 120_000), chapters[2].title);
  assert.equal(chapterLabel([], 60_000), undefined);
  assert.equal(
    chapterLabel([{ startMs: 60_000, title: 'Later' }], 59_999),
    undefined
  );
});
