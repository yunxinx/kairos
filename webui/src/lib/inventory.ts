import type { ChannelView, Price } from '@/api/types';

/** 别名映射：下游名 → 该渠道上游真名，并带来源渠道。 */
export interface AliasMapping {
  canonical: string;
  channelName: string;
}

/** 清单一行：一个可调用名（渠道 `models` ∪ 别名 key 的并集）。 */
export interface InventoryRow {
  name: string;
  channelNames: string[];
  aliases: AliasMapping[];
  price: Price | null;
}

export type InventoryLayout = 'unified' | 'by-channel';

/** 按渠道分组时的排版段；`channelName` 为 null 表示大一统列表。 */
export interface InventorySection {
  channelName: string | null;
  rows: InventoryRow[];
}

/** 从渠道与价格派生 Tab 1 清单；禁用渠道仍算已登记。 */
export function buildInventory(channels: ChannelView[], prices: Price[]): InventoryRow[] {
  const byName = new Map<string, InventoryRow>();
  const priceByModel = new Map(prices.map((price) => [price.model, price]));

  function rowOf(name: string): InventoryRow {
    const existing = byName.get(name);
    if (existing) return existing;
    const created: InventoryRow = {
      name,
      channelNames: [],
      aliases: [],
      price: priceByModel.get(name) ?? null,
    };
    byName.set(name, created);
    return created;
  }

  for (const channel of channels) {
    for (const model of channel.models) {
      const row = rowOf(model);
      if (!row.channelNames.includes(channel.name)) {
        row.channelNames.push(channel.name);
      }
    }
    for (const [alias, canonical] of Object.entries(channel.model_aliases)) {
      const row = rowOf(alias);
      if (!row.channelNames.includes(channel.name)) {
        row.channelNames.push(channel.name);
      }
      row.aliases.push({ canonical, channelName: channel.name });
    }
  }

  const rows = [...byName.values()];
  for (const row of rows) {
    row.channelNames.sort((left, right) => left.localeCompare(right));
    row.aliases.sort(
      (left, right) =>
        left.channelName.localeCompare(right.channelName) ||
        left.canonical.localeCompare(right.canonical),
    );
  }
  rows.sort((left, right) => left.name.localeCompare(right.name));
  return rows;
}

/** 目录对照用的查找 ID：别名行用真名，其余用可调用名本身。 */
export function catalogLookupId(row: InventoryRow): string {
  const first = row.aliases[0];
  return first ? first.canonical : row.name;
}

/** 「按渠道分组」只改排版：同一可调用名按其成员渠道出现在每一段。 */
export function sectionInventory(
  rows: InventoryRow[],
  layout: InventoryLayout,
): InventorySection[] {
  if (layout === 'unified') {
    return [{ channelName: null, rows }];
  }
  const byChannel = new Map<string, InventoryRow[]>();
  for (const row of rows) {
    for (const channelName of row.channelNames) {
      const list = byChannel.get(channelName);
      if (list) list.push(row);
      else byChannel.set(channelName, [row]);
    }
  }
  return [...byChannel.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([channelName, sectionRows]) => ({
      channelName,
      rows: [...sectionRows].sort((left, right) => left.name.localeCompare(right.name)),
    }));
}

/** 大一统与分段共用：按可调用名排序。 */
export function sortInventory(rows: InventoryRow[]): InventoryRow[] {
  return [...rows].sort((left, right) => left.name.localeCompare(right.name));
}
