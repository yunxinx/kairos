import type { ChannelView, ModelGroup, UnifiedModel } from '@/api/types';

export const DEFAULT_MODEL_GROUP = 'default';

/** 渠道已登记的可调用名：各渠道 `models` ∪ 别名 key（含禁用渠道）。 */
export function registeredCallableNames(channels: ChannelView[]): Set<string> {
  const names = new Set<string>();
  for (const channel of channels) {
    for (const model of channel.models) names.add(model);
    for (const alias of Object.keys(channel.model_aliases)) names.add(alias);
  }
  return names;
}

/**
 * 令牌绑定组是否允许调用该名。
 * 自定义组只看显式名单；`default` 另含未出现在任何其他组名单中的名字。
 */
export function groupAllows(groups: ModelGroup[], groupName: string, model: string): boolean {
  const group = groups.find((item) => item.name === groupName);
  if (group?.models.includes(model)) return true;
  if (groupName === DEFAULT_MODEL_GROUP) {
    return !groups.some((item) => item.name !== DEFAULT_MODEL_GROUP && item.models.includes(model));
  }
  return false;
}

export interface VisibleUnified {
  id: string;
  models: string[];
  hide: boolean;
  hiddenMembers: string[];
}

export interface VisiblePreview {
  visibleIds: string[];
  unified: VisibleUnified[];
}

/**
 * 当前分组在下游列表里能看见的 ID，以及统一模型展开顺序与隐藏结果。
 * 规则与后端 `visible_model_ids` 对齐，只读核对该组。
 */
export function previewVisibleModels(
  groups: ModelGroup[],
  unifiedModels: UnifiedModel[],
  channels: ChannelView[],
  groupName: string,
): VisiblePreview {
  const names = registeredCallableNames(channels);
  for (const unified of unifiedModels) names.add(unified.id);
  const group = groups.find((item) => item.name === groupName);
  if (group) {
    for (const model of group.models) names.add(model);
  }

  const allowed = [...names].filter((name) => groupAllows(groups, groupName, name));
  const allowedSet = new Set(allowed);

  const unifiedInGroup = unifiedModels
    .filter((model) => allowedSet.has(model.id))
    .sort((left, right) => left.id.localeCompare(right.id))
    .map((model) => {
      const hiddenMembers = model.hide ? model.models.filter((member) => member !== model.id) : [];
      return {
        id: model.id,
        models: [...model.models],
        hide: model.hide,
        hiddenMembers,
      };
    });

  const hiddenMembers = new Set(
    unifiedInGroup.flatMap((model) => (model.hide ? model.hiddenMembers : [])),
  );
  const visibleIds = allowed.filter((name) => !hiddenMembers.has(name)).sort();

  return { visibleIds, unified: unifiedInGroup };
}

/** 下游可见按分组排版的一段。 */
export interface VisibleSection {
  groupName: string;
  visibleIds: string[];
  unified: VisibleUnified[];
}

/**
 * 未选分组时展示全部非空组（类似清单「按渠道分组」）；
 * 勾选后只保留这些组，空组也留下便于对照。
 */
export function previewVisibleSections(
  groups: ModelGroup[],
  unifiedModels: UnifiedModel[],
  channels: ChannelView[],
  selectedGroupNames: string[],
): VisibleSection[] {
  const selected = new Set(selectedGroupNames);
  const names = selected.size > 0 ? [...selected] : groups.map((group) => group.name);
  names.sort((left, right) => {
    if (left === DEFAULT_MODEL_GROUP) return -1;
    if (right === DEFAULT_MODEL_GROUP) return 1;
    return left.localeCompare(right);
  });
  return names
    .map((groupName) => {
      const preview = previewVisibleModels(groups, unifiedModels, channels, groupName);
      return { groupName, visibleIds: preview.visibleIds, unified: preview.unified };
    })
    .filter((section) => selected.size > 0 || section.visibleIds.length > 0);
}
