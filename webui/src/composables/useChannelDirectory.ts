import { computed } from 'vue';
import { useQuery, type QueryClient } from '@tanstack/vue-query';
import { apiClient } from '@/api/client';
import type { ChannelSummary } from '@/api/types';
import { useCurrentUser } from '@/lib/session';

/** 渠道名录的缓存键；与 root-only 的完整定义 `['channels']` 分开。 */
export const CHANNEL_SUMMARY_KEY = ['channel-summaries'] as const;

/**
 * 改动渠道定义后刷新两份缓存。
 *
 * 完整定义与名录是同一份数据的两个投影，只失效其中一个会让另一个继续渲染旧的
 * 渠道名与启用态——例如刚停用的渠道在模型页仍显示正常。
 */
export async function invalidateChannelCaches(queryClient: QueryClient): Promise<void> {
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: ['channels'] }),
    queryClient.invalidateQueries({ queryKey: [...CHANNEL_SUMMARY_KEY] }),
  ]);
}

/**
 * 渠道名录：渲染判断（某个名挂在哪条渠道、渠道还在不在）统一走这里。
 *
 * 刻意不复用 `['channels']`：那份是 root-only 的完整定义（含密钥与出站地址），
 * admin 拿 403 后缓存为空数组，于是模型页把「我看不到渠道」渲染成了「渠道已失效」。
 * 名录端点对 admin+ 开放，`known` 则如实区分「还没到手」与「确实没有渠道」，
 * 让 UI 在不知情时闭嘴而不是编造失效状态。
 */
export function useChannelDirectory() {
  const me = useCurrentUser();

  const enabled = computed(() => me.value?.role === 'admin' || me.value?.role === 'root');

  const query = useQuery({
    queryKey: [...CHANNEL_SUMMARY_KEY],
    queryFn: () => apiClient.listChannelSummaries(),
    enabled,
  });

  const channels = computed<ChannelSummary[]>(() => query.data.value ?? []);
  /** 渠道表是否已经到手；未到手时不得对成员来源下判断。 */
  const channelsKnown = computed(() => query.data.value !== undefined);

  return { query, channels, channelsKnown, enabled };
}
