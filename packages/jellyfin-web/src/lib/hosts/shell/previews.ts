import React from 'react';
import { settings, useSetting } from '../../settings';
import { useLatest } from '../../use-latest';
import type { SourceInfo } from '../../types';
import {
  previewEligible,
  type PreviewImage,
} from '../../playback/preview-policy';

export interface SeekPreviews {
  enabled: boolean;
  setEnabled(value: boolean): void;
  status: string;
  session: string | null;
  images: ReadonlyMap<number, PreviewImage>;
  request(ms: number | null): void;
  report(
    event: 'cache-hit' | 'approximate' | 'miss' | 'display',
    ms: number,
    elapsed: number
  ): void;
}

export function useSeekPreviews(
  source: SourceInfo,
  url: string,
  external: boolean
): SeekPreviews | undefined {
  const [enabled, setEnabled] = useSetting(settings.desktop.seekPreviews);
  const eligible =
    !!window.aiostreamsDesktop?.seekPreviews &&
    !external &&
    previewEligible(source);
  const [connection, setConnection] = React.useState<{
    url: string;
    id: string;
  } | null>(null);
  const session = connection?.url === url ? connection.id : null;
  const [images, setImages] = React.useState<ReadonlyMap<number, PreviewImage>>(
    new Map()
  );
  const [status, setStatus] = React.useState('waiting');
  const latest = useLatest({ session, enabled, eligible });

  React.useEffect(() => {
    const shell = window.aiostreamsDesktop;
    setImages(new Map());
    setConnection(null);
    if (!shell?.seekPreviews) return;
    if (!eligible || !enabled) {
      const kind = source.aiostreams?.type;
      const reason = !enabled
        ? 'disabled'
        : external
          ? 'external'
          : source.IsInfiniteStream
            ? 'infinite'
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
    const unsubscribe = shell.subscribe((m) => {
      if (
        (m.type !== 'preview-frame' && m.type !== 'preview-status') ||
        m.session !== id
      )
        return;
      if (m.type === 'preview-status') setStatus(m.state);
      else
        setImages((old) => {
          const next = new Map(old);
          next.delete(m.bucket);
          next.set(m.bucket, m);
          while (
            next.size > 96 ||
            [...next.values()].reduce(
              (sum, frame) => sum + frame.image.length,
              0
            ) >
              8 * 1024 * 1024
          )
            next.delete(next.keys().next().value!);
          return next;
        });
    });
    shell.send({ type: 'preview-start', session: id, url });
    return () => {
      unsubscribe();
      shell.send({ type: 'preview-stop', session: id });
    };
  }, [eligible, enabled, url]);

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
  const report = React.useCallback(
    (event: string, ms: number, elapsed: number) => {
      const current = latest.current;
      if (!current.session || !current.enabled || !current.eligible) return;
      window.aiostreamsDesktop?.send({
        type: 'preview-report',
        session: current.session,
        event,
        position: ms / 1000,
        elapsed_ms: elapsed,
      });
    },
    [latest]
  );
  return eligible
    ? {
        enabled,
        setEnabled,
        status,
        session,
        images: session ? images : new Map(),
        request,
        report,
      }
    : undefined;
}
