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
  covers_until: number;
  covers_from?: number;
  image: string;
  aspect_ratio?: number;
}

/** A keyframe preview represents the interval between its sampled and requested times.
 * The UI labels the actual sampled time; half a step covers the bucket edges. */
export function closestPreview(
  images: ReadonlyMap<number, PreviewImage>,
  ms: number,
  stepMs = 10_000
): PreviewImage | null {
  if (!Number.isFinite(ms) || !Number.isFinite(stepMs) || stepMs <= 0)
    return null;
  let best: PreviewImage | null = null;
  let distance = Infinity;
  const tolerance = stepMs / 2;
  for (const image of images.values()) {
    if (
      !Number.isFinite(image.position) ||
      !Number.isFinite(image.covers_until)
    )
      continue;
    const sampled = image.position * 1000;
    const start =
      Math.min(image.position, image.covers_from ?? image.position) * 1000;
    const end = Math.max(image.position, image.covers_until) * 1000;
    const delta = Math.abs(sampled - ms);
    if (ms >= start - tolerance && ms <= end + tolerance && delta < distance) {
      best = image;
      distance = delta;
    }
  }
  return best;
}

export function validPreviewStep(ms: number): boolean {
  return Number.isInteger(ms) && ms >= 5000 && ms <= 30000 && ms % 5000 === 0;
}
