import type { SourceInfo } from '../types';

export type PreviewMode = 'full' | 'demand-only' | 'off';
export const MAX_PREVIEW_IMAGES = 600;
export const MAX_PREVIEW_CACHE_BYTES = 24 * 1024 * 1024;

/** Metadata distinguishes cached direct streams from expensive on-demand sources. */
export function previewMode(source: SourceInfo): PreviewMode {
  if (source.IsInfiniteStream || source.SupportsDirectPlay === false)
    return 'off';
  if (source.aiostreams?.type === 'usenet') return 'full';
  if (source.aiostreams?.type === 'http') return 'full';
  return source.aiostreams?.type === 'debrid' &&
    source.aiostreams.cached === true
    ? 'full'
    : 'off';
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
    mode: previewMode(source),
    kind: ['debrid', 'usenet', 'http'].includes(source.aiostreams?.type ?? '')
      ? source.aiostreams!.type
      : 'unknown',
    colour: colours[video?.VideoRangeType ?? ''] ?? 'unknown',
    dv_profile:
      Number.isInteger(video?.DvProfile) &&
      video!.DvProfile! >= 0 &&
      video!.DvProfile! <= 15
        ? video!.DvProfile
        : null,
    width:
      Number.isInteger(video?.Width) &&
      video!.Width! > 0 &&
      video!.Width! <= 32768
        ? video!.Width
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
  stepMs = 5_000
): PreviewImage | null {
  if (!Number.isFinite(ms) || !Number.isFinite(stepMs) || stepMs <= 0)
    return null;
  let best: PreviewImage | null = null;
  let distance = Infinity;
  // Match the native one-microsecond allowance for floating-point slot edges.
  const tolerance = stepMs / 2 + 0.001;
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
  // Up to 24 hours / 240 positions; accept the native millisecond precision.
  return Number.isInteger(ms) && ms >= 5000 && ms <= 360000;
}

/** One uninterrupted pointer run: keep its geometry and unanswered demand clock
 * across bucket changes. Neither recovery nor a stale image starts a new run. */
export class PreviewInteraction {
  private frame: boolean | null = null;
  private pendingSince: number | null = null;

  begin(enabled: boolean, status: string): boolean {
    this.frame ??= enabled && status !== 'unavailable';
    return this.frame;
  }

  request(at: number): void {
    this.pendingSince ??= at;
  }

  elapsed(at: number): number | undefined {
    return this.pendingSince === null
      ? undefined
      : Math.max(0, at - this.pendingSince);
  }

  answered(at: number): number | undefined {
    const elapsed = this.elapsed(at);
    this.pendingSince = null;
    return elapsed;
  }

  leave(): void {
    this.frame = null;
    this.pendingSince = null;
  }
}
