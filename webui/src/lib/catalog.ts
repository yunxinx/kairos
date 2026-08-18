import type { CatalogModel, Price } from '@/api/types';
import { channelModelKey } from '@/lib/inventory';

/** 填价策略：只填空白档，或覆盖已有单价。 */
export type CatalogFillMode = 'blanks' | 'overwrite';

export interface CatalogFillSource {
  model: string;
  channelId: number;
  channelName: string;
  lookupId: string;
  price: Price | null;
}

export type CatalogFillStatus = 'will-write' | 'no-match' | 'need-host' | 'unchanged';

export interface CatalogHostOption {
  value: string;
  label: string;
}

/** 运营者为人选的目录行：提供方 + 模型 id。 */
export interface CatalogPick {
  providerId: string;
  modelId: string;
}

export interface CatalogFillPreview {
  model: string;
  channelId: number;
  channelName: string;
  lookupId: string;
  hits: CatalogModel[];
  hostOptions: CatalogHostOption[];
  /** 唯一提供方时的展示名；多个提供方或未命中为 null。 */
  hostName: string | null;
  selected: CatalogPick | null;
  nextPrice: Price | null;
  status: CatalogFillStatus;
}

export function catalogSourceKey(channelId: number, model: string): string {
  return channelModelKey(channelId, model);
}

export function catalogRowKey(model: Pick<CatalogModel, 'provider_id' | 'model_id'>): string {
  return `${model.provider_id}:${model.model_id}`;
}

/** 按 model_id 收集全部提供方命中；多个提供方必须由运营者人选。 */
export function findCatalogHits(catalog: CatalogModel[], modelId: string): CatalogModel[] {
  return catalog
    .filter((item) => item.model_id === modelId)
    .sort(
      (left, right) =>
        left.provider_id.localeCompare(right.provider_id) ||
        left.model_id.localeCompare(right.model_id),
    );
}

/**
 * 按策略把目录四档写入渠道价。
 * `blanks` 只填当前空档；`overwrite` 用目录价覆盖已填档（目录缺该项则保持现状）。
 * 新建价格行仍需要 input/output。
 */
export function applyCatalogPrice(
  model: string,
  channelId: number,
  existing: Price | null,
  catalog: CatalogModel,
  mode: CatalogFillMode,
): Price | null {
  const inputMicros = pickRequiredTier(mode, catalog.input_micros, existing?.input_micros);
  const outputMicros = pickRequiredTier(mode, catalog.output_micros, existing?.output_micros);
  if (inputMicros === null || outputMicros === null) return null;
  return {
    channel_id: channelId,
    model,
    input_micros: inputMicros,
    output_micros: outputMicros,
    cache_read_micros: pickOptionalTier(
      mode,
      catalog.cache_read_micros,
      existing?.cache_read_micros ?? null,
    ),
    cache_write_micros: pickOptionalTier(
      mode,
      catalog.cache_write_micros,
      existing?.cache_write_micros ?? null,
    ),
  };
}

function pickRequiredTier(
  mode: CatalogFillMode,
  fromCatalog: number | null,
  current: number | undefined,
): number | null {
  if (mode === 'overwrite') {
    return fromCatalog ?? current ?? null;
  }
  return current ?? fromCatalog;
}

function pickOptionalTier(
  mode: CatalogFillMode,
  fromCatalog: number | null,
  current: number | null,
): number | null {
  if (mode === 'overwrite') {
    return fromCatalog ?? current;
  }
  return current ?? fromCatalog;
}

function pricesEqual(left: Price, right: Price): boolean {
  return (
    left.channel_id === right.channel_id &&
    left.model === right.model &&
    left.input_micros === right.input_micros &&
    left.output_micros === right.output_micros &&
    left.cache_read_micros === right.cache_read_micros &&
    left.cache_write_micros === right.cache_write_micros
  );
}

function hostPresentation(hits: CatalogModel[]): {
  hostOptions: CatalogHostOption[];
  hostName: string | null;
} {
  return {
    hostOptions: hits.map((hit) => ({
      value: hit.provider_id,
      label: hit.provider_name,
    })),
    hostName: hits.length === 1 ? (hits[0]?.provider_name ?? null) : null,
  };
}

function resolvePicked(
  catalog: CatalogModel[],
  hits: CatalogModel[],
  pick: CatalogPick | undefined,
): CatalogModel | null {
  if (pick) {
    return (
      catalog.find(
        (item) => item.provider_id === pick.providerId && item.model_id === pick.modelId,
      ) ?? null
    );
  }
  if (hits.length === 1) return hits[0] ?? null;
  return null;
}

/** 为清单行生成目录填价预览；多个提供方未选则标 `need-host`，对不上则 `no-match`。 */
export function buildCatalogFillPreview(
  sources: CatalogFillSource[],
  catalog: CatalogModel[],
  picks: Record<string, CatalogPick>,
  mode: CatalogFillMode,
): CatalogFillPreview[] {
  return sources.map((source) => {
    const hits = findCatalogHits(catalog, source.lookupId);
    const hosts = hostPresentation(hits);
    const identity = {
      model: source.model,
      channelId: source.channelId,
      channelName: source.channelName,
      lookupId: source.lookupId,
    };
    const key = catalogSourceKey(source.channelId, source.model);
    const picked = resolvePicked(catalog, hits, picks[key]);
    if (!picked) {
      return {
        ...identity,
        hits,
        ...hosts,
        selected: null,
        nextPrice: null,
        status: (hits.length > 1 ? 'need-host' : 'no-match') as CatalogFillStatus,
      };
    }
    const nextPrice = applyCatalogPrice(source.model, source.channelId, source.price, picked, mode);
    const unchanged =
      nextPrice !== null && source.price !== null && pricesEqual(source.price, nextPrice);
    return {
      ...identity,
      hits,
      ...hosts,
      selected: { providerId: picked.provider_id, modelId: picked.model_id },
      nextPrice,
      status: nextPrice === null ? 'no-match' : unchanged ? 'unchanged' : 'will-write',
    };
  });
}
