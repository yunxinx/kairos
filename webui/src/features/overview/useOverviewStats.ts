import { computed, ref, type ComputedRef, type Ref } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { apiClient, extractApiError } from '@/api/client';
import type { DailyPoint, LifetimeStats, StatsSummary } from '@/api/types';
import { OVERVIEW_DEFAULT_DAYS } from '@/lib/admin-query-defaults';

export interface OverviewShareRow {
  name: string;
  requestCount: number;
  costUsdMicros: number;
}

export function useOverviewStats(): {
  days: Ref<string>;
  summary: ComputedRef<StatsSummary | undefined>;
  daily: ComputedRef<DailyPoint[]>;
  byModel: ComputedRef<OverviewShareRow[]>;
  byChannel: ComputedRef<OverviewShareRow[]>;
  lifetime: ComputedRef<LifetimeStats | null>;
  lifetimeLoading: ComputedRef<boolean>;
  lifetimeError: ComputedRef<string>;
  statsErrorMessage: ComputedRef<string>;
  showSkeleton: ComputedRef<boolean>;
  showError: ComputedRef<boolean>;
  retryStats: () => void;
  retryLifetime: () => void;
} {
  const days = ref(String(OVERVIEW_DEFAULT_DAYS));

  const statsQuery = useQuery({
    queryKey: ['stats', days],
    queryFn: () => apiClient.getStats(Number(days.value)),
  });

  const lifetimeQuery = useQuery({
    queryKey: ['stats', 'lifetime'],
    queryFn: () => apiClient.getLifetimeStats(),
  });

  const summary = computed(() => statsQuery.data.value?.summary);
  const daily = computed(() => statsQuery.data.value?.daily ?? []);

  const byModel = computed((): OverviewShareRow[] =>
    (statsQuery.data.value?.by_model ?? []).map((share) => ({
      name: share.model,
      requestCount: share.request_count,
      costUsdMicros: share.cost_usd_micros,
    })),
  );

  const byChannel = computed((): OverviewShareRow[] =>
    (statsQuery.data.value?.by_channel ?? []).map((share) => ({
      name: share.channel,
      requestCount: share.request_count,
      costUsdMicros: share.cost_usd_micros,
    })),
  );

  const lifetime = computed(() => lifetimeQuery.data.value ?? null);
  const lifetimeLoading = computed(
    () => lifetimeQuery.isPending.value && !lifetimeQuery.data.value,
  );
  const lifetimeError = computed(() => {
    if (!lifetimeQuery.isError.value || lifetimeQuery.data.value) return '';
    return extractApiError(lifetimeQuery.error.value).message;
  });

  const statsErrorMessage = computed(() => {
    if (!statsQuery.isError.value) return '';
    return extractApiError(statsQuery.error.value).message;
  });

  const showSkeleton = computed(() => statsQuery.isPending.value && !statsQuery.data.value);
  const showError = computed(() => statsQuery.isError.value && !statsQuery.data.value);

  function retryStats(): void {
    void statsQuery.refetch();
  }

  function retryLifetime(): void {
    void lifetimeQuery.refetch();
  }

  return {
    days,
    summary,
    daily,
    byModel,
    byChannel,
    lifetime,
    lifetimeLoading,
    lifetimeError,
    statsErrorMessage,
    showSkeleton,
    showError,
    retryStats,
    retryLifetime,
  };
}
