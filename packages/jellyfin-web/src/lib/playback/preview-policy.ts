import type { SourceInfo } from '../types';

/** An HTTP URL alone also covers Usenet and future P2P proxies; require metadata. */
export function previewEligible(source: SourceInfo): boolean {
  return (
    source.aiostreams?.type === 'debrid' &&
    source.aiostreams.cached === true &&
    !source.IsInfiniteStream &&
    source.SupportsDirectPlay !== false
  );
}

export interface PreviewImage {
  bucket: number;
  position: number;
  image: string;
}

/** Never present a distant cached scene as the requested position. */
export function closestPreview(
  images: ReadonlyMap<number, PreviewImage>,
  ms: number
): PreviewImage | null {
  let best: PreviewImage | null = null;
  let distance = 15_000;
  for (const image of images.values()) {
    const delta = Math.abs(image.position * 1000 - ms);
    if (delta <= distance) {
      best = image;
      distance = delta;
    }
  }
  return best;
}
