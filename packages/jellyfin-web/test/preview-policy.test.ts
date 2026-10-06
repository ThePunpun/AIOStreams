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
    [0, { bucket: 0, position: 0, image: 'first' }],
    [30, { bucket: 30, position: 58, image: 'later' }],
  ]);
  assert.equal(closestPreview(images, 60_000)?.image, 'later');
  assert.equal(closestPreview(images, 35_000), null);
  assert.equal(closestPreview(new Map(), 1_000), null);
});
