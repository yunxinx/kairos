import { channelWriteBody, type Channel, type ChannelView, type Price } from '@/api/types';

/** 别名列上的关联名及其来源渠道。 */
export interface AliasMapping {
  alias: string;
  channelName: string;
}

/** 清单一行：渠道 `models` 里的可调用名。上游真名若不在清单中，不成行。 */
export interface InventoryRow {
  name: string;
  channelNames: string[];
  /** 指向本行的下游别名（本行是清单里的主模型）。 */
  aliases: AliasMapping[];
  /** 本行作为「仅别名在清单」时的出站主模型。 */
  outbound: AliasMapping[];
  price: Price | null;
}

/** 按渠道分组时的排版段。 */
export interface InventorySection {
  channelName: string;
  rows: InventoryRow[];
}

function addChannel(row: InventoryRow, channelName: string) {
  if (!row.channelNames.includes(channelName)) {
    row.channelNames.push(channelName);
  }
}

function addRelated(list: AliasMapping[], alias: string, channelName: string) {
  if (list.some((item) => item.alias === alias && item.channelName === channelName)) return;
  list.push({ alias, channelName });
}

/**
 * 从渠道与价格派生 Tab 1 清单；禁用渠道仍算已登记。
 *
 * 成行只跟渠道 `models`（同步视图写入的清单）走：
 * - 别名 key 与其主模型都在清单 → 折叠进主模型行，别名列展示该 key；
 * - 只把别名放进清单（主模型不在 `models`）→ 行名是别名，别名列展示主模型；
 *   主模型本身不成行（路由也不按 alias value 匹配）。
 */
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
      outbound: [],
      price: priceByModel.get(name) ?? null,
    };
    byName.set(name, created);
    return created;
  }

  for (const channel of channels) {
    const inList = new Set(channel.models);

    for (const name of channel.models) {
      const canonical = channel.model_aliases[name];
      if (canonical !== undefined && inList.has(canonical)) {
        continue;
      }
      const row = rowOf(name);
      addChannel(row, channel.name);
      if (canonical !== undefined) {
        addRelated(row.outbound, canonical, channel.name);
      }
    }

    for (const [alias, canonical] of Object.entries(channel.model_aliases)) {
      if (!inList.has(canonical)) continue;
      const row = rowOf(canonical);
      addChannel(row, channel.name);
      addRelated(row.aliases, alias, channel.name);
    }
  }

  const rows = [...byName.values()];
  for (const row of rows) {
    row.channelNames.sort((left, right) => left.localeCompare(right));
    row.aliases.sort(compareRelated);
    row.outbound.sort(compareRelated);
  }
  rows.sort((left, right) => left.name.localeCompare(right.name));
  return rows;
}

function compareRelated(left: AliasMapping, right: AliasMapping): number {
  return left.channelName.localeCompare(right.channelName) || left.alias.localeCompare(right.alias);
}

/** 目录对照 ID：仅别名在清单时用出站主模型查 models.dev，否则用行名。 */
export function catalogLookupId(row: InventoryRow): string {
  return row.outbound[0]?.alias ?? row.name;
}

function relatedOnChannel(
  items: AliasMapping[],
  channelName: string | null | undefined,
): AliasMapping[] {
  if (channelName == null) return items;
  return items.filter((item) => item.channelName === channelName);
}

/** 别名列 chip：`canonical` 表示出站主模型（仅别名在清单）。 */
export interface AliasChip {
  name: string;
  canonical: boolean;
}

/** 别名列：下游别名，或仅别名在清单时的主模型；可按渠道收窄。 */
export function aliasChips(row: InventoryRow, channelName?: string | null): AliasChip[] {
  const chips: AliasChip[] = [];
  const seen = new Set<string>();
  for (const item of relatedOnChannel(row.aliases, channelName)) {
    if (seen.has(item.alias)) continue;
    seen.add(item.alias);
    chips.push({ name: item.alias, canonical: false });
  }
  for (const item of relatedOnChannel(row.outbound, channelName)) {
    if (seen.has(item.alias)) continue;
    seen.add(item.alias);
    chips.push({ name: item.alias, canonical: true });
  }
  return chips;
}

/**
 * 从指定渠道删除该行时，要从该渠道 `models` 拿掉的清单名：
 * 行名 + 该渠道上折叠进本行的别名 key。不含出站主模型（它不在本渠道清单里）。
 */
export function listedNamesOnChannel(row: InventoryRow, channelName: string): string[] {
  const names = [row.name];
  for (const item of row.aliases) {
    if (item.channelName === channelName) names.push(item.alias);
  }
  return names;
}

/** 从渠道写契约里拿掉指定名字（主模型与别名 key/value）。 */
export function stripNamesFromChannel(channel: ChannelView, drop: Set<string>): Channel {
  const models = channel.models.filter((name) => !drop.has(name));
  const model_aliases = Object.fromEntries(
    Object.entries(channel.model_aliases).filter(
      ([alias, canonical]) => !drop.has(alias) && !drop.has(canonical),
    ),
  );
  return {
    ...channelWriteBody(channel),
    models,
    model_aliases,
  };
}

/** 渠道清单或别名是否因 `stripNamesFromChannel` 发生变化。 */
export function channelChangedByStrip(channel: ChannelView, next: Channel): boolean {
  if (channel.models.length !== next.models.length) return true;
  if (Object.keys(channel.model_aliases).length !== Object.keys(next.model_aliases).length) {
    return true;
  }
  return channel.models.some((name, index) => name !== next.models[index]);
}

/** 按渠道分段：同一可调用名按其成员渠道出现在每一段。 */
export function sectionInventory(rows: InventoryRow[]): InventorySection[] {
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

/** 按可调用名排序。 */
export function sortInventory(rows: InventoryRow[]): InventoryRow[] {
  return [...rows].sort((left, right) => left.name.localeCompare(right.name));
}
