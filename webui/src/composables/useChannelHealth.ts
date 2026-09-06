import { computed } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { apiClient } from '@/api/client';
import type { ChannelHealthEntry } from '@/api/types';
import { useCurrentUser } from '@/lib/session';

/**
 * 渠道健康冷却视图：root-only 的进程内状态只读投影。
 *
 * 缓存键挂在渠道清单键之下（`['channels', 'health']`），渠道定义的任何失效
 * （保存、启停、删除）都会连同一并刷新；不引入独立轮询。非 root 会话不发请求、
 * 不展示，请求失败（如角色快照过期拿到 403）也只按无冷却渲染，不打断渠道表。
 */
export function useChannelHealth() {
  const me = useCurrentUser();

  const enabled = computed(() => me.value?.role === 'root');

  const query = useQuery({
    queryKey: ['channels', 'health'],
    queryFn: () => apiClient.getChannelHealth(),
    enabled,
  });

  /** channel_id → 冷却条目；仅含当前冷却中的渠道。 */
  const cooldowns = computed(() => {
    const map = new Map<number, ChannelHealthEntry>();
    for (const entry of query.data.value?.channels ?? []) {
      map.set(entry.channel_id, entry);
    }
    return map;
  });

  return { enabled, cooldowns };
}
