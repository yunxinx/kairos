import type { ChannelSummary, GroupModel, UnifiedModel } from '@/api/types';
import {
  channelNameForMember,
  memberSourceKind,
  type CallableSourceLine,
  type MemberSourceKind,
} from '@/lib/unified-sources';

/** 可用模型表一行：统一模型或某一渠道上的已登记名。 */
export type GroupPickRow =
  | { kind: 'unified'; name: string }
  | {
      kind: 'source';
      name: string;
      channelId: number;
      channelName: string;
      channelKind: MemberSourceKind;
    };

export function groupModelKey(entry: GroupModel): string {
  if (entry.kind === 'unified') return `unified:${entry.id}`;
  return `source:${entry.channel_id}:${entry.model}`;
}

export function groupModelName(entry: GroupModel): string {
  return entry.kind === 'unified' ? entry.id : entry.model;
}

export function groupPickKey(row: GroupPickRow): string {
  if (row.kind === 'unified') return `unified:${row.name}`;
  return `source:${row.channelId}:${row.name}`;
}

export function pickRowToMember(row: GroupPickRow): GroupModel {
  if (row.kind === 'unified') return { kind: 'unified', id: row.name };
  return { kind: 'source', channel_id: row.channelId, model: row.name };
}

export function pickRowIsMember(row: GroupPickRow, members: GroupModel[]): boolean {
  const key = groupPickKey(row);
  return members.some((member) => groupModelKey(member) === key);
}

/** 已登记名按渠道分行，另加统一模型行。 */
export function groupPickRows(
  channels: ChannelSummary[],
  unifiedModels: UnifiedModel[],
): GroupPickRow[] {
  const rows: GroupPickRow[] = [];
  for (const channel of channels) {
    const names = new Set([...channel.models, ...Object.keys(channel.model_aliases)]);
    for (const name of names) {
      rows.push({
        kind: 'source',
        name,
        channelId: channel.id,
        channelName: channel.name,
        channelKind: channel.enabled ? 'ok' : 'disabled',
      });
    }
  }
  for (const model of unifiedModels) {
    rows.push({ kind: 'unified', name: model.id });
  }
  rows.sort((left, right) => {
    const byName = left.name.localeCompare(right.name);
    if (byName !== 0) return byName;
    if (left.kind !== right.kind) return left.kind === 'unified' ? -1 : 1;
    if (left.kind === 'source' && right.kind === 'source') {
      return left.channelName.localeCompare(right.channelName);
    }
    return 0;
  });
  return rows;
}

function sourceLine(
  key: string,
  name: string,
  channels: CallableSourceLine['channels'],
  emptyKind?: MemberSourceKind,
): CallableSourceLine {
  const line: CallableSourceLine = {
    key,
    name,
    isUnified: false,
    channels,
    unifiedMembers: [],
  };
  if (emptyKind !== undefined) line.emptyKind = emptyKind;
  return line;
}

export function groupMemberSourceLine(
  member: GroupModel,
  channels: ChannelSummary[],
  unifiedModels: UnifiedModel[],
  channelsKnown = true,
): CallableSourceLine {
  if (member.kind === 'unified') {
    const unified = unifiedModels.find((model) => model.id === member.id);
    return {
      key: groupModelKey(member),
      name: member.id,
      isUnified: true,
      channels: [],
      unifiedMembers: unified?.models ?? [],
    };
  }
  const kind = memberSourceKind(member, channels, channelsKnown);
  const channelName = channelNameForMember(channels, member);
  return sourceLine(
    groupModelKey(member),
    member.model,
    channelName === '' ? [] : [{ name: channelName, kind }],
    kind,
  );
}

/** 组表「组内模型」：一条名单对应一行。 */
export function groupModelDisplayLines(
  models: GroupModel[],
  channels: ChannelSummary[],
  unifiedModels: UnifiedModel[],
  channelsKnown = true,
): CallableSourceLine[] {
  return models.map((member) =>
    groupMemberSourceLine(member, channels, unifiedModels, channelsKnown),
  );
}
