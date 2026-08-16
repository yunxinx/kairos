import type { Price } from '@/api/types';
import { parseUsdToMicros } from '@/lib/format';

/** models.dev 公开目录；匹配必须带 providerId，禁止只按裸 ID 跨提供方写入。 */
export const MODELS_DEV_CATALOG_URL = 'https://models.dev/api.json';

export interface CatalogCost {
  input?: number;
  output?: number;
  cache_read?: number;
  cache_write?: number;
}

export interface CatalogHit {
  providerId: string;
  providerName: string;
  modelId: string;
  cost: CatalogCost;
}

interface CatalogModel {
  id?: string;
  cost?: CatalogCost;
}

interface CatalogProvider {
  id?: string;
  name?: string;
  models?: Record<string, CatalogModel>;
}

export type CatalogFile = Record<string, CatalogProvider>;

/** 拉取 models.dev 目录。CORS 已开放 `*`，管理面可直连。 */
export async function fetchModelsDevCatalog(): Promise<CatalogFile> {
  const response = await fetch(MODELS_DEV_CATALOG_URL);
  if (!response.ok) {
    throw new Error(`models.dev catalog HTTP ${response.status}`);
  }
  return (await response.json()) as CatalogFile;
}

/** 按 modelId 收集全部提供方命中；多个提供方必须由运营者人选。 */
export function findCatalogHits(catalog: CatalogFile, modelId: string): CatalogHit[] {
  const hits: CatalogHit[] = [];
  for (const [providerKey, provider] of Object.entries(catalog)) {
    const model = provider.models?.[modelId];
    if (!model?.cost) continue;
    hits.push({
      providerId: provider.id ?? providerKey,
      providerName: provider.name ?? provider.id ?? providerKey,
      modelId,
      cost: model.cost,
    });
  }
  hits.sort((left, right) => left.providerId.localeCompare(right.providerId));
  return hits;
}

/** 目录美元/1M tokens → micro-USD；非法或负值视为不可用。 */
export function catalogDollarsToMicros(value: number | undefined): number | null {
  if (value === undefined || !Number.isFinite(value) || value < 0) return null;
  return parseUsdToMicros(value.toFixed(6));
}

/**
 * 按勾选档位写入目录价，允许覆盖已填单价。
 * 未勾选的档位保持现状；目录缺该项时也保持现状。
 * 新建价格行仍需要 input/output：未勾选且无现价则无法写入。
 */
export function applyCatalogTiers(
  model: string,
  existing: Price | null,
  cost: CatalogCost,
  tiers: ReadonlySet<CatalogTier>,
): Price | null {
  const inputMicros = pickRequiredTier(tiers.has('input'), cost.input, existing?.input_micros);
  const outputMicros = pickRequiredTier(tiers.has('output'), cost.output, existing?.output_micros);
  if (inputMicros === null || outputMicros === null) return null;
  return {
    model,
    input_micros: inputMicros,
    output_micros: outputMicros,
    cache_read_micros: pickOptionalTier(
      tiers.has('cacheRead'),
      cost.cache_read,
      existing?.cache_read_micros ?? null,
    ),
    cache_write_micros: pickOptionalTier(
      tiers.has('cacheWrite'),
      cost.cache_write,
      existing?.cache_write_micros ?? null,
    ),
  };
}

function pickRequiredTier(
  selected: boolean,
  catalog: number | undefined,
  current: number | undefined,
): number | null {
  if (selected) {
    return catalogDollarsToMicros(catalog) ?? current ?? null;
  }
  return current ?? null;
}

function pickOptionalTier(
  selected: boolean,
  catalog: number | undefined,
  current: number | null,
): number | null {
  if (!selected) return current;
  const fromCatalog = catalogDollarsToMicros(catalog);
  return fromCatalog !== null ? fromCatalog : current;
}

export const CATALOG_TIERS = ['input', 'output', 'cacheRead', 'cacheWrite'] as const;
export type CatalogTier = (typeof CATALOG_TIERS)[number];

export interface CatalogFillSource {
  model: string;
  lookupId: string;
  price: Price | null;
}

export type CatalogFillStatus = 'will-write' | 'no-match' | 'need-host' | 'unchanged';

export interface CatalogHostOption {
  value: string;
  label: string;
}

export interface CatalogFillPreview {
  model: string;
  lookupId: string;
  hits: CatalogHit[];
  hostOptions: CatalogHostOption[];
  /** 唯一提供方时的展示名；多个提供方或未命中为 null。 */
  hostName: string | null;
  selectedProviderId: string | null;
  nextPrice: Price | null;
  status: CatalogFillStatus;
}

function hostPresentation(hits: CatalogHit[]): {
  hostOptions: CatalogHostOption[];
  hostName: string | null;
} {
  return {
    hostOptions: hits.map((hit) => ({
      value: hit.providerId,
      label: hit.providerName,
    })),
    hostName: hits.length === 1 ? (hits[0]?.providerName ?? null) : null,
  };
}

function pricesEqual(left: Price, right: Price): boolean {
  return (
    left.model === right.model &&
    left.input_micros === right.input_micros &&
    left.output_micros === right.output_micros &&
    left.cache_read_micros === right.cache_read_micros &&
    left.cache_write_micros === right.cache_write_micros
  );
}

/** 为勾选的清单行生成目录填价预览；多个提供方未选则标 `need-host`。勾选档位允许覆盖已填单价。 */
export function buildCatalogFillPreview(
  sources: CatalogFillSource[],
  catalog: CatalogFile,
  hostPicks: Record<string, string>,
  tiers: ReadonlySet<CatalogTier>,
): CatalogFillPreview[] {
  return sources.map((source) => {
    const hits = findCatalogHits(catalog, source.lookupId);
    const hosts = hostPresentation(hits);
    if (hits.length === 0) {
      return {
        model: source.model,
        lookupId: source.lookupId,
        hits,
        ...hosts,
        selectedProviderId: null,
        nextPrice: null,
        status: 'no-match',
      };
    }
    const auto = hits.length === 1 ? hits[0] : undefined;
    const picked = auto ?? hits.find((hit) => hit.providerId === hostPicks[source.model]);
    if (!picked) {
      return {
        model: source.model,
        lookupId: source.lookupId,
        hits,
        ...hosts,
        selectedProviderId: null,
        nextPrice: null,
        status: 'need-host',
      };
    }
    if (tiers.size === 0) {
      return {
        model: source.model,
        lookupId: source.lookupId,
        hits,
        ...hosts,
        selectedProviderId: picked.providerId,
        nextPrice: source.price,
        status: source.price ? 'unchanged' : 'no-match',
      };
    }
    const nextPrice = applyCatalogTiers(source.model, source.price, picked.cost, tiers);
    const unchanged =
      nextPrice !== null && source.price !== null && pricesEqual(source.price, nextPrice);
    return {
      model: source.model,
      lookupId: source.lookupId,
      hits,
      ...hosts,
      selectedProviderId: picked.providerId,
      nextPrice,
      status: nextPrice === null ? 'no-match' : unchanged ? 'unchanged' : 'will-write',
    };
  });
}
