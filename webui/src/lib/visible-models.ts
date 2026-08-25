import type {
  ChannelSummary,
  ManagementRole,
  ModelGroup,
  UnifiedMember,
  UnifiedModel,
} from '@/api/types';
import { groupModelName } from '@/lib/group-models';

export const DEFAULT_MODEL_GROUP = 'default';

export function groupDisplayName(name: string, ungroupedLabel: string): string {
  return name === DEFAULT_MODEL_GROUP ? ungroupedLabel : name;
}

/** 令牌/渠道下拉：内置 default 显示为未分组，其余按名列出。 */
export function groupSelectOptions(
  groups: ModelGroup[],
  current: string,
  ungroupedLabel: string,
): { value: string; label: string }[] {
  const options = [
    { value: DEFAULT_MODEL_GROUP, label: ungroupedLabel },
    ...groups
      .filter((group) => group.name !== DEFAULT_MODEL_GROUP)
      .map((group) => group.name)
      .sort((left, right) => left.localeCompare(right))
      .map((name) => ({ value: name, label: name })),
  ];
  if (current && !options.some((item) => item.value === current)) {
    options.push({ value: current, label: current });
  }
  return options;
}

/**
 * 令牌当前绑定的组是否仍能调用。
 * 空组始终失效；普通用户还要求组仍在可用名单里。admin/root 只要组名非空（组存在性由保存接口校验）。
 */
export function tokenGroupUsable(
  modelGroup: string,
  role: ManagementRole,
  assignedGroups: readonly string[],
): boolean {
  if (modelGroup === '') return false;
  if (role === 'root') return true;
  return assignedGroups.includes(modelGroup);
}

/** 普通用户：只列出被分配的组名。编辑已绑且已撤的组时，把当前值附在末尾以便提示失效。 */
export function assignedGroupOptions(
  groups: string[],
  current: string,
  ungroupedLabel: string,
  keepCurrentIfMissing = true,
): { value: string; label: string }[] {
  const options = [...groups]
    .sort((left, right) => left.localeCompare(right))
    .map((name) => ({
      value: name,
      label: name === DEFAULT_MODEL_GROUP ? ungroupedLabel : name,
    }));
  if (keepCurrentIfMissing && current && !options.some((item) => item.value === current)) {
    options.push({
      value: current,
      label: current === DEFAULT_MODEL_GROUP ? ungroupedLabel : current,
    });
  }
  return options;
}

/** 渠道已登记的可调用名：各渠道 `models` ∪ 别名 key（含禁用渠道）。 */
export function registeredCallableNames(channels: ChannelSummary[]): Set<string> {
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
  if (group?.models.some((entry) => groupModelName(entry) === model)) return true;
  if (groupName === DEFAULT_MODEL_GROUP) {
    return !groups.some(
      (item) =>
        item.name !== DEFAULT_MODEL_GROUP &&
        item.models.some((entry) => groupModelName(entry) === model),
    );
  }
  return false;
}

export interface VisibleUnified {
  id: string;
  models: UnifiedMember[];
  hide: boolean;
  /** hide 开启时被从下游列表拿掉的成员（保留钉死渠道，不含与统一 ID 同名的成员）。 */
  hiddenMembers: UnifiedMember[];
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
  channels: ChannelSummary[],
  groupName: string,
): VisiblePreview {
  const names = registeredCallableNames(channels);
  for (const unified of unifiedModels) names.add(unified.id);
  const group = groups.find((item) => item.name === groupName);
  if (group) {
    for (const entry of group.models) names.add(groupModelName(entry));
  }

  const allowed = [...names].filter((name) => groupAllows(groups, groupName, name));
  const allowedSet = new Set(allowed);

  const unifiedInGroup = unifiedModels
    .filter((model) => allowedSet.has(model.id))
    .sort((left, right) => left.id.localeCompare(right.id))
    .map((model) => {
      const hiddenMembers = model.hide
        ? model.models.filter((member) => member.model !== model.id)
        : [];
      return {
        id: model.id,
        models: [...model.models],
        hide: model.hide,
        hiddenMembers,
      };
    });

  const hiddenNames = new Set(
    unifiedInGroup.flatMap((model) =>
      model.hide ? model.hiddenMembers.map((member) => member.model) : [],
    ),
  );
  const visibleIds = allowed.filter((name) => !hiddenNames.has(name)).sort();

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
  channels: ChannelSummary[],
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
