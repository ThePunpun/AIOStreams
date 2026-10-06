import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  previewEligible,
  closestPreview,
} from '../src/lib/playback/preview-policy.ts';

test('only cached debrid is eligible, including HTTP-proxied Usenet and P2P', () => {
  const source = (type: string, cached = true) =>
    ({
      aiostreams: { type, cached },
      Path: 'https://example.test/video.mkv',
    }) as Parameters<typeof previewEligible>[0];
  assert.equal(previewEligible(source('debrid')), true);
  for (const type of ['usenet', 'p2p', 'http', 'live'])
    assert.equal(previewEligible(source(type)), false);
  assert.equal(previewEligible(source('debrid', false)), false);
  assert.equal(
    previewEligible({ Path: 'https://example.test/video.mkv' }),
    false
  );
  assert.equal(
    previewEligible({ ...source('debrid'), IsInfiniteStream: true }),
    false
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
});

test('invalid timestamps do not produce cached previews', () => {
  const images = new Map([
    [0, { bucket: 0, position: 0, covers_until: NaN, image: 'bad' }],
  ]);
  assert.equal(closestPreview(images, 0), null);
  assert.equal(closestPreview(images, NaN), null);
  assert.equal(closestPreview(new Map(), 0, 0), null);
});
