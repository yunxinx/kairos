/** 模型清单的排序与比较：编辑器 chip、同步表格与保存载荷共用同一顺序约定。 */

/** 模型自然顺序：数字段按数值比较（如 gpt-9 排在 gpt-10 前）。 */
export function compareModels(left: string, right: string): number {
  return left.localeCompare(right, undefined, { numeric: true, sensitivity: 'base' });
}

/** 两个模型集合是否相同（不看顺序），用于脏状态判定。 */
export function sameModelSet(left: string[], right: string[]): boolean {
  if (left.length !== right.length) return false;
  const sortedLeft = [...left].sort();
  const sortedRight = [...right].sort();
  return sortedLeft.every((model, index) => model === sortedRight[index]);
}

/** 渠道表格「模型清单」列的一条：清单可调用名或仅存在于别名表的昵称，附带别名关系。 */
export interface ListedModelChip {
  name: string;
  /** 该名是别名 key 时的出站主模型。 */
  actualRequest?: string;
  /** 该名是主模型时，指向它的下游别名。 */
  aliases: string[];
}

/**
 * 渠道展示 chip：`models` 里的可调用名，加上尚未列入 `models` 的别名 key。
 * 别名 key 带出站主模型；主模型带其别名列表（别名本身不在清单时仍挂在主模型上）。
 */
export function listedModelChips(
  models: string[],
  aliases: Record<string, string>,
): ListedModelChip[] {
  const listed = new Set(models);
  const canonicalAliases = new Map<string, string[]>();
  for (const [alias, canonical] of Object.entries(aliases)) {
    const list = canonicalAliases.get(canonical);
    if (list) list.push(alias);
    else canonicalAliases.set(canonical, [alias]);
  }
  const chips: ListedModelChip[] = models.map((name) => {
    const canonical = aliases[name];
    if (canonical !== undefined) {
      return { name, actualRequest: canonical, aliases: [] };
    }
    return { name, aliases: canonicalAliases.get(name) ?? [] };
  });
  for (const [alias, canonical] of Object.entries(aliases)) {
    if (listed.has(alias)) continue;
    chips.push({ name: alias, actualRequest: canonical, aliases: [] });
  }
  chips.sort((left, right) => compareModels(left.name, right.name));
  return chips;
}

/** 两个别名映射是否相同（不看键序），用于脏状态判定。 */
export function sameAliasMap(left: Record<string, string>, right: Record<string, string>): boolean {
  const leftKeys = Object.keys(left);
  if (leftKeys.length !== Object.keys(right).length) return false;
  return leftKeys.every((alias) => Object.hasOwn(right, alias) && right[alias] === left[alias]);
}

/** 同步视图提交时的一行：勾选态、仅别名生效、以及该主模型名下的别名草稿。 */
export interface SyncListingRow {
  name: string;
  aliases: string[];
  selected: boolean;
  aliasOnly: boolean;
}

/** 别名草稿与已勾选主模型名冲突：占用另一主模型，或同一别名指向两个主模型。 */
export type SyncListingConflict =
  | {
      kind: 'occupies_selected';
      alias: string;
      owner: string;
      occupied: string;
    }
  | {
      kind: 'claimed_twice';
      alias: string;
      first: string;
      second: string;
    };

/** 同步视图「保存并返回」的提交结果。 */
export type SyncListingCommit =
  | { ok: true; models: string[]; aliases: Record<string, string> }
  | { ok: false; conflict: SyncListingConflict };

/**
 * 把同步表格的勾选与别名草稿收成渠道 `models` / `model_aliases`。
 *
 * 与自身同名的别名忽略。别名占用另一已勾选主模型名、或同一别名指向两个
 * 主模型时拒绝：否则保存后该主模型会被改写成别名，路由按别名出站。
 */
export function commitSyncListing(rows: SyncListingRow[]): SyncListingCommit {
  const selected = rows.filter((row) => row.selected);
  const selectedNames = new Set(selected.map((row) => row.name));
  const claimed = new Map<string, string>();

  for (const row of selected) {
    for (const alias of row.aliases) {
      if (alias === row.name) continue;
      if (selectedNames.has(alias)) {
        return {
          ok: false,
          conflict: {
            kind: 'occupies_selected',
            alias,
            owner: row.name,
            occupied: alias,
          },
        };
      }
      const existing = claimed.get(alias);
      if (existing !== undefined && existing !== row.name) {
        return {
          ok: false,
          conflict: {
            kind: 'claimed_twice',
            alias,
            first: existing,
            second: row.name,
          },
        };
      }
      claimed.set(alias, row.name);
    }
  }

  const models = new Set<string>();
  const aliases: Record<string, string> = {};
  for (const row of selected) {
    const mapped = row.aliases.filter((alias) => alias !== row.name);
    const listedAsCanonical = !row.aliasOnly || mapped.length === 0;
    if (listedAsCanonical) models.add(row.name);
    for (const alias of mapped) {
      // 主模型已在清单时别名只写映射：再写入 models 会变成占用形（K 与 C 都在清单）。
      if (!listedAsCanonical) models.add(alias);
      aliases[alias] = row.name;
    }
  }
  return { ok: true, models: [...models].sort(compareModels), aliases };
}

/** 连通性探测表格的一行：展示名与出站主模型名。 */
export interface ProbeModelRow {
  displayName: string;
  probeModel: string;
}

/**
 * 从清单去重出探测行：按主模型名分组。
 * 主模型名在清单则只展示主模型名；仅别名在清单则展示清单中出现的第一个别名。
 */
export function probeModelRows(models: string[], aliases: Record<string, string>): ProbeModelRow[] {
  const canonicalOf = (name: string): string => aliases[name] ?? name;
  const groups = new Map<string, string[]>();
  for (const name of models) {
    const canonical = canonicalOf(name);
    const list = groups.get(canonical);
    if (list) {
      list.push(name);
    } else {
      groups.set(canonical, [name]);
    }
  }
  const rows: ProbeModelRow[] = [];
  for (const [canonical, names] of groups) {
    const displayName = names.includes(canonical) ? canonical : (names[0] ?? canonical);
    rows.push({ displayName, probeModel: canonical });
  }
  return rows;
}
