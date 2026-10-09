import React from 'react';
import { settings, useSetting } from '../../settings';
import { useLatest } from '../../use-latest';
import type { SourceInfo } from '../../types';
import {
  previewMode,
  previewSource,
  validPreviewStep,
  MAX_PREVIEW_IMAGES,
  MAX_PREVIEW_CACHE_BYTES,
  type PreviewImage,
} from '../../playback/preview-policy';

export interface SeekPreviews {
  enabled: boolean;
  setEnabled(value: boolean): void;
  status: string;
  stepMs: number;
  aspectRatio: number | null;
  hover(active: boolean): void;
  session: string | null;
  images: ReadonlyMap<number, PreviewImage>;
  request(ms: number | null): void;
  report(
    event:
      | 'miss'
      | 'display-cached'
      | 'display-cold'
      | 'timeline-enter'
      | 'timeline-leave',
    ms: number,
    elapsed: number,
    sampledMs?: number,
    hoverToImageMs?: number
  ): void;
}

export function useSeekPreviews(
  source: SourceInfo,
  url: string,
  external: boolean
): SeekPreviews | undefined {
  const [enabled, setEnabled] = useSetting(settings.desktop.seekPreviews);
  const mode = previewMode(source);
  const kind = source.aiostreams?.type;
  const eligible =
    !!window.aiostreamsDesktop?.seekPreviews && !external && mode !== 'off';
  const [connection, setConnection] = React.useState<{
    url: string;
    id: string;
  } | null>(null);
  const session = connection?.url === url ? connection.id : null;
  const [images, setImages] = React.useState<ReadonlyMap<number, PreviewImage>>(
    new Map()
  );
  const [status, setStatus] = React.useState('waiting');
  const [stepMs, setStepMs] = React.useState(5_000);
  const [aspectRatio, setAspectRatio] = React.useState<number | null>(null);
  const latest = useLatest({ session, enabled, eligible });

  React.useEffect(() => {
    const shell = window.aiostreamsDesktop;
    setImages(new Map());
    setConnection(null);
    if (!shell?.seekPreviews) return;
    if (!eligible || !enabled) {
      const reason = !enabled
        ? 'disabled'
        : external
          ? 'external'
          : source.IsInfiniteStream
            ? 'live'
            : source.SupportsDirectPlay === false
              ? 'no-direct-play'
              : kind === 'debrid'
                ? 'debrid-uncached'
                : ['usenet', 'p2p', 'live'].includes(kind ?? '')
                  ? kind
                  : 'unknown';
      shell.send({
        type: 'preview-report',
        session: crypto.randomUUID(),
        event: 'source-skipped',
        reason,
        position: 0,
        elapsed_ms: 0,
      });
      return;
    }
    const id = crypto.randomUUID();
    setConnection({ url, id });
    setStatus('waiting');
    setStepMs(5_000);
    setAspectRatio(null);
    const unsubscribe = shell.subscribe((m) => {
      if (
        (m.type !== 'preview-frame' && m.type !== 'preview-status') ||
        m.session !== id
      )
        return;
      if (m.type === 'preview-status') {
        setStatus(m.state);
        if (validPreviewStep(m.step_ms)) setStepMs(m.step_ms);
        if (
          m.aspect_ratio &&
          Number.isFinite(m.aspect_ratio) &&
          m.aspect_ratio >= 0.1 &&
          m.aspect_ratio <= 10
        )
          setAspectRatio(m.aspect_ratio);
      } else
        setImages((old) => {
          const next = new Map(old);
          next.delete(m.bucket);
          next.set(m.bucket, m);
          while (
            next.size > MAX_PREVIEW_IMAGES ||
            [...next.values()].reduce(
              (sum, frame) => sum + frame.image.length,
              0
            ) > MAX_PREVIEW_CACHE_BYTES
          )
            next.delete(next.keys().next().value!);
          return next;
        });
    });
    shell.send({
      type: 'preview-start',
      session: id,
      url,
      source: previewSource(source),
    });
    return () => {
      unsubscribe();
      shell.send({ type: 'preview-stop', session: id });
    };
  }, [eligible, enabled, url, mode, kind]);

  const request = React.useCallback(
    (ms: number | null) => {
      const current = latest.current;
      if (!current.session || !current.enabled || !current.eligible) return;
      window.aiostreamsDesktop?.send({
        type: 'preview-request',
        session: current.session,
        position: ms === null ? null : ms / 1000,
      });
    },
    [latest]
  );
  const hover = React.useCallback(
    (active: boolean) => {
      const current = latest.current;
      if (!current.session || !current.enabled || !current.eligible) return;
      window.aiostreamsDesktop?.send({
        type: 'preview-hover',
        session: current.session,
        active,
      });
    },
    [latest]
  );
  const report = React.useCallback(
    (
      event: string,
      ms: number,
      elapsed: number,
      sampledMs?: number,
      hoverToImageMs?: number
    ) => {
      const current = latest.current;
      if (!current.session || !current.enabled || !current.eligible) return;
      window.aiostreamsDesktop?.send({
        type: 'preview-report',
        session: current.session,
        event,
        position: ms / 1000,
        elapsed_ms: elapsed,
        sampled_position: sampledMs === undefined ? null : sampledMs / 1000,
        hover_to_image_ms: hoverToImageMs,
      });
    },
    [latest]
  );
  return eligible
    ? {
        enabled,
        setEnabled,
        status,
        stepMs,
        aspectRatio,
        hover,
        session,
        images: session ? images : new Map(),
        request,
        report,
      }
    : undefined;
}
