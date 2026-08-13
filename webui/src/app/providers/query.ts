import { keepPreviousData, QueryClient } from '@tanstack/vue-query';

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      retry: false,
      /** 换 queryKey 时保留上一份数据，避免骨架把已渲染界面拆掉再撑开。 */
      placeholderData: keepPreviousData,
    },
  },
});
