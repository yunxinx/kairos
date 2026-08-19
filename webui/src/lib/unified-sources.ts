import type { ChannelView, UnifiedMember, UnifiedModel } from '@/api/types';

export function unifiedMemberKey(member: Pick<UnifiedMember, 'channel_id' | 'model'>): string {
  return `${member.channel_id}:${member.model}`;
}

/** 渠道是否把该名当作可调用名（清单条目或别名 key）。 */
function channelListsCallable(channel: ChannelView, model: string): boolean {
  return channel.models.includes(model) || Object.hasOwn(channel.model_aliases, model);
}

/** 成员钉死的渠道名；渠道已删则空串。 */
export function channelNameForMember(channels: ChannelView[], member: UnifiedMember): string {
  return channels.find((channel) => channel.id === member.channel_id)?.name ?? '';
}

/** 统一模型是否有任一成员钉在该渠道上。 */
export function unifiedUsesChannel(
  members: UnifiedMember[],
  channels: ChannelView[],
  channelName: string,
): boolean {
  const id = channels.find((channel) => channel.name === channelName)?.id;
  return id !== undefined && members.some((member) => member.channel_id === id);
}

/** 钉渠道成员相对当前渠道表的来源状态。 */
export type MemberSourceKind = 'ok' | 'unlisted' | 'disabled' | 'gone';

const MEMBER_SOURCE_I18N = {
  unlisted: 'models.memberSourceUnlisted',
  disabled: 'models.memberSourceDisabled',
  gone: 'models.memberSourceGone',
} as const;

/** 状态标文案 key；正常不标。 */
export function memberSourceI18nKey(
  kind: MemberSourceKind,
): (typeof MEMBER_SOURCE_I18N)[Exclude<MemberSourceKind, 'ok'>] | undefined {
  if (kind === 'ok') return undefined;
  return MEMBER_SOURCE_I18N[kind];
}

/**
 * 渠道已删 → gone；还在但不登记该名 → unlisted；登记了但停用 → disabled。
 * 停用且未登记时优先 unlisted（缺模型比停用更具体）。
 */
export function memberSourceKind(
  member: Pick<UnifiedMember, 'channel_id' | 'model'>,
  channels: ChannelView[],
): MemberSourceKind {
  const channel = channels.find((item) => item.id === member.channel_id);
  if (channel === undefined) return 'gone';
  if (!channelListsCallable(channel, member.model)) return 'unlisted';
  if (!channel.enabled) return 'disabled';
  return 'ok';
}

/**
 * 可调用名的渠道路由预览：priority 升序、同级 weight 降序（高权重更常排前）、再按渠道名。
 * 与运行时加权随机不同，这里给出稳定的偏好顺序；禁用渠道仍列出。
 */
export function callableRouteMembers(model: string, channels: ChannelView[]): UnifiedMember[] {
  return channels
    .filter((channel) => channelListsCallable(channel, model))
    .sort((left, right) => {
      if (left.priority !== right.priority) return left.priority - right.priority;
      if (left.weight !== right.weight) return right.weight - left.weight;
      return left.name.localeCompare(right.name);
    })
    .map((channel) => ({
      channel_id: channel.id,
      model,
    }));
}

/** 普通可调用名当前挂着的渠道及启用状态。 */
export interface SourceChannel {
  name: string;
  kind: MemberSourceKind;
}

function sourceChannelsForCallable(name: string, channels: ChannelView[]): SourceChannel[] {
  const byName = new Map<string, MemberSourceKind>();
  for (const channel of channels) {
    if (!channelListsCallable(channel, name)) continue;
    const kind: MemberSourceKind = channel.enabled ? 'ok' : 'disabled';
    const previous = byName.get(channel.name);
    if (previous === undefined || previous !== 'ok') byName.set(channel.name, kind);
  }
  return [...byName.entries()]
    .map(([channelName, kind]) => ({ name: channelName, kind }))
    .sort((left, right) => left.name.localeCompare(right.name));
}

/** 组内/可选列表一行：统一模型带成员，普通名带当前来源渠道。 */
export interface CallableSourceLine {
  name: string;
  isUnified: boolean;
  channels: SourceChannel[];
  unifiedMembers: UnifiedMember[];
}

export function callableSourceLine(
  name: string,
  channels: ChannelView[],
  unifiedModels: UnifiedModel[],
): CallableSourceLine {
  const unified = unifiedModels.find((model) => model.id === name);
  const isUnified = unified !== undefined;
  return {
    name,
    isUnified,
    channels: isUnified ? [] : sourceChannelsForCallable(name, channels),
    unifiedMembers: unified?.models ?? [],
  };
}
