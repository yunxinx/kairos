import { computed, ref, watch, type ComputedRef, type Ref } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { apiClient, extractApiError } from '@/api/client';
import type { DailyPoint, LifetimeStats, StatsSummary } from '@/api/types';
import { OVERVIEW_DEFAULT_DAYS } from '@/lib/admin-query-defaults';
import { readValidatedNumber, writePlainNumber } from '@/lib/preferences';

export interface OverviewShareRow {
  name: string;
  requestCount: number;
  costUsdMicros: number;
}

const OVERVIEW_DAYS_OPTIONS = [1, 7, 30, 90] as const;
const OVERVIEW_DAYS_KEY = 'kairos-overview-days';

export { OVERVIEW_DAYS_OPTIONS };

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
  // 天数档位属个人偏好，落 localStorage（合法档位校验，非法值回落默认 7 天）。
  const days = ref(
    String(readValidatedNumber(OVERVIEW_DAYS_KEY, OVERVIEW_DAYS_OPTIONS, OVERVIEW_DEFAULT_DAYS)),
  );

  watch(days, (next) => {
    const parsed = Number.parseInt(next, 10);
    if (OVERVIEW_DAYS_OPTIONS.includes(parsed as (typeof OVERVIEW_DAYS_OPTIONS)[number])) {
      writePlainNumber(OVERVIEW_DAYS_KEY, parsed);
    }
  });

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
