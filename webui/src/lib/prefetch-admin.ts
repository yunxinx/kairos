import { queryClient } from '@/app/providers/query';
import { apiClient } from '@/api/client';
import { loadTokenRows } from '@/api/token-rows';

/** 导航悬停时预取对应页数据，进入时尽量已有缓存、不再拆布局。 */
export function prefetchAdminRoute(to: string): void {
  switch (to) {
    case '/overview':
      void queryClient.prefetchQuery({
        queryKey: ['stats', '7'],
        queryFn: () => apiClient.getStats(7),
      });
      void queryClient.prefetchQuery({
        queryKey: ['stats', 'lifetime'],
        queryFn: () => apiClient.getLifetimeStats(),
      });
      return;
    case '/token':
      void queryClient.prefetchQuery({
        queryKey: ['tokens'],
        queryFn: loadTokenRows,
      });
      return;
    case '/channel':
      void queryClient.prefetchQuery({
        queryKey: ['channels'],
        queryFn: () => apiClient.listChannels(),
      });
      return;
    case '/pricing':
      void queryClient.prefetchQuery({
        queryKey: ['prices'],
        queryFn: () => apiClient.listPrices(),
      });
      return;
    case '/config':
      void queryClient.prefetchQuery({
        queryKey: ['settings'],
        queryFn: () => apiClient.getSettings(),
      });
      return;
    case '/requests':
      void queryClient.prefetchQuery({
        queryKey: ['logs', 1, 20, '', '', '', ''],
        queryFn: () => apiClient.queryLogs({ page: 1, page_size: 20 }),
      });
      return;
    default:
  }
}
