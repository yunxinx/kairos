import type { ChannelModelOrder, ChannelView, UnifiedMember } from '@/api/types';

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
 * 可调用名的渠道路由预览：同名顺序表升序，未显式排序的候选按渠道 id 兜底。
 * 与运行时选路一致，这里给出稳定顺序；禁用渠道仍列出。
 */
export function callableRouteMembers(
  model: string,
  channels: ChannelView[],
  orders: ChannelModelOrder[] = [],
): UnifiedMember[] {
  const order = orders.find((entry) => entry.model === model);
  const positions = new Map<number, number>();
  order?.channel_ids.forEach((channelId, index) => positions.set(channelId, index));
  return channels
    .filter((channel) => channelListsCallable(channel, model))
    .sort((left, right) => {
      const leftPosition = positions.get(left.id) ?? Number.MAX_SAFE_INTEGER;
      const rightPosition = positions.get(right.id) ?? Number.MAX_SAFE_INTEGER;
      if (leftPosition !== rightPosition) return leftPosition - rightPosition;
      return left.id - right.id;
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

/** 组内/可选列表一行：统一模型带成员，普通名带钉渠道。 */
export interface CallableSourceLine {
  /** 列表/网格去重键：须含渠道，避免同名跨渠道撞车。 */
  key: string;
  name: string;
  isUnified: boolean;
  channels: SourceChannel[];
  unifiedMembers: UnifiedMember[];
  /** 没有渠道 chip 时的状态标；缺省未登记。 */
  emptyKind?: MemberSourceKind;
}
