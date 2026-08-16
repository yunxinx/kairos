import type { Price } from '@/api/types';
import { parseUsdToMicros } from '@/lib/format';

/** models.dev 公开目录；匹配必须带 providerId，禁止只按裸 ID 跨宿主写入。 */
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

/** 按 modelId 收集全部宿主命中；多宿主必须由运营者人选。 */
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
 * 只填空档：已有价格行的 input/output 不改；缓存 `null` 视为空档可填。
 * 无价格行时用目录创建；缺少 input/output 则无法写入。
 */
export function fillEmptyTiers(
  model: string,
  existing: Price | null,
  cost: CatalogCost,
): Price | null {
  const inputMicros = existing?.input_micros ?? catalogDollarsToMicros(cost.input);
  const outputMicros = existing?.output_micros ?? catalogDollarsToMicros(cost.output);
  if (inputMicros === null || outputMicros === null) return null;
  return {
    model,
    input_micros: inputMicros,
    output_micros: outputMicros,
    cache_read_micros:
      existing && existing.cache_read_micros !== null
        ? existing.cache_read_micros
        : catalogDollarsToMicros(cost.cache_read),
    cache_write_micros:
      existing && existing.cache_write_micros !== null
        ? existing.cache_write_micros
        : catalogDollarsToMicros(cost.cache_write),
  };
}

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
  /** 唯一宿主时的展示名；多宿主或未命中为 null。 */
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

/** 为勾选的清单行生成目录填价预览；多宿主未选则标 `need-host`。 */
export function buildCatalogFillPreview(
  sources: CatalogFillSource[],
  catalog: CatalogFile,
  hostPicks: Record<string, string>,
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
    const nextPrice = fillEmptyTiers(source.model, source.price, picked.cost);
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
