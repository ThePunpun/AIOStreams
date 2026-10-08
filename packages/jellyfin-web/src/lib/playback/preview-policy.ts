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

/** A hint only: the native worker checks the selected decoded track too. */
export function previewSource(source: SourceInfo) {
  const videos =
    source.MediaStreams?.filter((stream) => stream.Type === 'Video') ?? [];
  const defaults = videos.filter((stream) => stream.IsDefault);
  const video =
    videos.length === 1
      ? videos[0]
      : defaults.length === 1
        ? defaults[0]
        : undefined;
  const colours: Record<string, string> = {
    SDR: 'sdr',
    HDR10: 'pq',
    HDR10Plus: 'pq',
    HLG: 'hlg',
    DOVIWithHDR10: 'dv-pq',
    DOVIWithHDR10Plus: 'dv-pq',
    DOVIWithEL: 'dv-pq',
    DOVIWithELHDR10Plus: 'dv-pq',
    DOVIWithHLG: 'dv-hlg',
    DOVIWithSDR: 'dv-sdr',
    DOVI: 'dv',
  };
  return {
    colour: colours[video?.VideoRangeType ?? ''] ?? 'unknown',
    dv_profile:
      Number.isInteger(video?.DvProfile) &&
      video!.DvProfile! >= 0 &&
      video!.DvProfile! <= 15
        ? video!.DvProfile
        : null,
    bitrate:
      Number.isFinite(video?.BitRate) && video!.BitRate! > 0
        ? video!.BitRate
        : null,
  };
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
 * Sampled time stays in metadata; half a step covers the bucket edges. */
export function closestPreview(
  images: ReadonlyMap<number, PreviewImage>,
  ms: number,
  stepMs = 8_000
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
  return Number.isInteger(ms) && ms >= 5000 && ms <= 10000 && ms % 1000 === 0;
}
