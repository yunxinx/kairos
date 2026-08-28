import { queryClient } from '@/app/providers/query';
import { apiClient } from '@/api/client';
import { loadTokenRows } from '@/api/token-rows';
import { CHANNEL_SUMMARY_KEY } from '@/composables/useChannelDirectory';
import {
  LOGS_INITIAL_PAGE,
  LOGS_INITIAL_PAGE_SIZE,
  LOGS_INITIAL_QUERY_KEY,
  OVERVIEW_DEFAULT_DAYS,
} from '@/lib/admin-query-defaults';
import { getMe } from '@/lib/session';

/** 导航悬停时预取对应页数据，进入时尽量已有缓存、不再拆布局。 */
export function prefetchAdminRoute(to: string): void {
  switch (to) {
    case '/overview':
      void queryClient.prefetchQuery({
        queryKey: ['stats', String(OVERVIEW_DEFAULT_DAYS)],
        queryFn: () => apiClient.getStats(OVERVIEW_DEFAULT_DAYS),
      });
      void queryClient.prefetchQuery({
        queryKey: ['stats', 'lifetime'],
        queryFn: () => apiClient.getLifetimeStats(),
      });
      return;
    case '/tokens':
      void queryClient.prefetchQuery({
        queryKey: ['tokens'],
        queryFn: loadTokenRows,
      });
      return;
    case '/channels':
      void queryClient.prefetchQuery({
        queryKey: ['channels'],
        queryFn: () => apiClient.listChannels(),
      });
      return;
    case '/plans':
      void queryClient.prefetchQuery({
        queryKey: ['plans'],
        queryFn: () => apiClient.listPlans(),
      });
      void queryClient.prefetchQuery({
        queryKey: ['model-groups'],
        queryFn: () => apiClient.listModelGroups(),
      });
      void queryClient.prefetchQuery({
        queryKey: ['channel-model-orders'],
        queryFn: () => apiClient.listChannelModelOrders(),
      });
      return;
    case '/models':
      // 普通用户的模型页是另一条数据源：下面四个端点对他全是 403，预取只会白吃四个。
      if (getMe()?.role === 'user') {
        void queryClient.prefetchQuery({
          queryKey: ['my-models'],
          queryFn: () => apiClient.listMyModels(),
        });
        return;
      }
      // 模型页只按名录渲染；完整定义是 root-only，预取它会让 admin 白吃一个 403。
      void queryClient.prefetchQuery({
        queryKey: [...CHANNEL_SUMMARY_KEY],
        queryFn: () => apiClient.listChannelSummaries(),
      });
      void queryClient.prefetchQuery({
        queryKey: ['prices'],
        queryFn: () => apiClient.listPrices(),
      });
      void queryClient.prefetchQuery({
        queryKey: ['unified-models'],
        queryFn: () => apiClient.listUnifiedModels(),
      });
      void queryClient.prefetchQuery({
        queryKey: ['model-groups'],
        queryFn: () => apiClient.listModelGroups(),
      });
      return;
    case '/settings':
      void queryClient.prefetchQuery({
        queryKey: ['settings'],
        queryFn: () => apiClient.getSettings(),
      });
      void queryClient.prefetchQuery({
        queryKey: ['catalog-meta'],
        queryFn: () => apiClient.getCatalogMeta(),
      });
      return;
    case '/logs':
      void queryClient.prefetchQuery({
        queryKey: LOGS_INITIAL_QUERY_KEY,
        queryFn: () =>
          apiClient.queryLogs({ page: LOGS_INITIAL_PAGE, page_size: LOGS_INITIAL_PAGE_SIZE }),
      });
      return;
    case '/users':
      void queryClient.prefetchQuery({
        queryKey: ['users'],
        queryFn: () => apiClient.listUsers(),
      });
      return;
    default:
  }
}
