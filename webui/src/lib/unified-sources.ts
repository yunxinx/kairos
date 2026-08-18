import type { ChannelView, UnifiedMember } from '@/api/types';

export function unifiedMemberKey(member: Pick<UnifiedMember, 'channel_id' | 'model'>): string {
  return `${member.channel_id}:${member.model}`;
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
