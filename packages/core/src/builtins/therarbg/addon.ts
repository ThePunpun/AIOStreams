import { BaseDebridAddon, BaseDebridConfigSchema } from '../base/debrid.js';
import { z } from 'zod';
import {
  createLogger,
  getTimeTakenSincePoint,
  ParsedId,
} from '../../utils/index.js';
import TheRARBGAPI, { TheRARBGCategory, getTheRARBGUrl } from './api.js';
import { NZB, UnprocessedTorrent } from '../../debrid/utils.js';
import { validateInfoHash } from '../utils/debrid.js';
import { config as appConfig } from '../../config/index.js';
import { createQueryLimit, getTitleLanguagesForUrl } from '../utils/general.js';

const logger = createLogger('therarbg');

export const TheRARBGAddonConfigSchema = BaseDebridConfigSchema;

export type TheRARBGAddonConfig = z.infer<typeof TheRARBGAddonConfigSchema>;

export class TheRARBGAddon extends BaseDebridAddon<TheRARBGAddonConfig> {
  readonly id = 'therarbg';
  readonly name = 'TheRARBG';
  readonly version = '1.0.0';
  readonly logger = logger;
  readonly api: TheRARBGAPI;

  constructor(userData: TheRARBGAddonConfig, clientIp?: string) {
    super(userData, TheRARBGAddonConfigSchema, clientIp);
    this.api = new TheRARBGAPI();
  }

  protected async _searchNzbs(_parsedId: ParsedId): Promise<NZB[]> {
    return [];
  }

  protected async _searchTorrents(
    parsedId: ParsedId
  ): Promise<UnprocessedTorrent[]> {
    const queryLimit = createQueryLimit();
    const metadata = await this.getSearchMetadata();
    if (!metadata.primaryTitle) return [];

    const titleQueries = this.buildQueries(parsedId, metadata, {
      titleLanguages: getTitleLanguagesForUrl(getTheRARBGUrl(), this.id),
    });
    if (titleQueries.length === 0 && !metadata.imdbId) return [];

    const categories = [
      ...(parsedId.mediaType === 'movie' ? [TheRARBGCategory.Movies] : []),
      ...(parsedId.mediaType === 'series'
        ? [TheRARBGCategory.TV, TheRARBGCategory.TVShows]
        : []),
      ...(metadata.isAnime ? [TheRARBGCategory.Anime] : []),
    ];

    const runQueries = async (queryList: string[]) =>
      (
        await Promise.all(
          queryList.map((q) =>
            queryLimit(async () => {
              const start = Date.now();
              logger.debug(`Fetching first page for query "${q}"`);
              const firstPageResponse = await this.api.search({
                query: q,
                page: 1,
                categories,
              });
              const { total, pageSize } = firstPageResponse;
              let allResults = [...firstPageResponse.results];
              const totalPages = Math.min(
                Math.ceil(total / pageSize),
                appConfig.builtins.therarbg.pageLimit
              );
              if (totalPages > 1) {
                const pageNumbers = Array.from(
                  { length: totalPages - 1 },
                  (_, i) => i + 2
                );
                const remainingResults = await Promise.all(
                  pageNumbers.map(async (pageNum) => {
                    const { results } = await this.api.search({
                      query: q,
                      page: pageNum,
                      categories,
                    });
                    return results;
                  })
                );
                allResults.push(...remainingResults.flat());
              }
              logger.info(
                `TheRARBG search for ${q} took ${getTimeTakenSincePoint(start)}`,
                { results: allResults.length, pages: Math.max(totalPages, 1) }
              );
              return allResults;
            })
          )
        )
      ).flat();

    logger.info(`Performing TheRARBG search`, { queries: titleQueries, categories });
    let titleResults = await runQueries(titleQueries);

    const yearlessFallback = appConfig.builtins.scrape.yearlessMovieFallback;
    if (
      parsedId.mediaType === 'movie' &&
      metadata.year &&
      yearlessFallback.enabled
    ) {
      const uniqueCount = new Set(
        titleResults.map((result) => result.hash ?? result.name)
      ).size;
      if (uniqueCount < yearlessFallback.resultThreshold) {
        const yearSuffix = ` ${metadata.year}`;
        const yearlessQueries = [
          ...new Set(
            titleQueries
              .filter((q) => q.endsWith(yearSuffix))
              .map((q) => q.slice(0, -yearSuffix.length).trim())
              .filter(Boolean)
          ),
        ];
        if (yearlessQueries.length > 0) {
          logger.info(
            'Year-constrained TheRARBG movie search returned too few unique results; retrying without year',
            {
              uniqueResults: uniqueCount,
              threshold: yearlessFallback.resultThreshold,
              queries: yearlessQueries,
            }
          );
          titleResults.push(...(await runQueries(yearlessQueries)));
        }
      }
    }

    const imdbResults = metadata.imdbId ? await runQueries([metadata.imdbId]) : [];
    const results = [...titleResults, ...imdbResults].filter(
      (result) =>
        !result.imdbId ||
        !metadata.imdbId ||
        result.imdbId === metadata.imdbId
    );

    const seenTorrents = new Set<string>();
    const torrents: UnprocessedTorrent[] = [];
    for (const result of results) {
      const hash = validateInfoHash(result.hash);
      if (!hash) {
        logger.warn(`TheRARBG search hit has no hash: ${JSON.stringify(result)}`);
        continue;
      }
      const downloadUrl = `https://itorrents.org/${hash.toUpperCase()}.torrent?title=${result.name}`;
      if (seenTorrents.has(hash)) continue;
      seenTorrents.add(hash);
      const age = Math.ceil(
        (Date.now() - result.age * 1000) / (1000 * 60 * 60)
      );
      torrents.push({
        hash,
        downloadUrl,
        sources: [],
        indexer: `TheRARBG | ${result.user}`,
        seeders: result.seeders,
        age,
        title: result.name,
        size: result.size,
        type: 'torrent',
      });
    }
    return torrents;
  }
}
