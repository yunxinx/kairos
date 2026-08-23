import { queryClient } from '@/app/providers/query';
import { apiClient } from '@/api/client';
import { loadTokenRows } from '@/api/token-rows';
import {
  LOGS_INITIAL_PAGE,
  LOGS_INITIAL_PAGE_SIZE,
  LOGS_INITIAL_QUERY_KEY,
  OVERVIEW_DEFAULT_DAYS,
} from '@/lib/admin-query-defaults';

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
      return;
    case '/models':
      void queryClient.prefetchQuery({
        queryKey: ['channels'],
        queryFn: () => apiClient.listChannels(),
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
