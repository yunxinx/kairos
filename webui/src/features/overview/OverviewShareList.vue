<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import EmptyState from '@/components/ui/EmptyState.vue';
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';
import { formatCount, formatPercent, formatUsdMicros } from '@/lib/format';

interface OverviewShareItem {
  name: string;
  requestCount: number;
  costUsdMicros: number;
}

type ShareTab = 'model' | 'channel';

const SHARE_LIST_LIMIT = 5;

const props = withDefaults(
  defineProps<{
    modelItems: OverviewShareItem[];
    channelItems: OverviewShareItem[];
    loading?: boolean;
  }>(),
  {
    loading: false,
  },
);

const { t, locale } = useI18n();
const shareTab = ref<ShareTab>('model');

const rankedAll = computed(() => {
  const items = shareTab.value === 'model' ? props.modelItems : props.channelItems;
  return [...items].sort(
    (left, right) =>
      right.costUsdMicros - left.costUsdMicros || right.requestCount - left.requestCount,
  );
});

const ranked = computed(() => rankedAll.value.slice(0, SHARE_LIST_LIMIT));

const itemTestId = computed(() =>
  shareTab.value === 'model' ? 'overview-model-share' : 'overview-channel-share',
);

const nameAttr = computed(() => (shareTab.value === 'model' ? 'data-model' : 'data-channel'));

const totalCost = computed(() =>
  rankedAll.value.reduce((sum, item) => sum + item.costUsdMicros, 0),
);

const totalRequests = computed(() =>
  rankedAll.value.reduce((sum, item) => sum + item.requestCount, 0),
);

function shareRatio(item: OverviewShareItem): number {
  if (totalCost.value > 0) {
    return item.costUsdMicros / totalCost.value;
  }
  if (totalRequests.value > 0) {
    return item.requestCount / totalRequests.value;
  }
  return 0;
}

function nameBinding(name: string): Record<string, string> {
  return { [nameAttr.value]: name };
}
</script>

<template>
  <div class="card overview-panel">
    <div class="card-header">
      <h2 class="min-w-0 truncate font-serif text-base font-semibold">
        {{ t('overview.shareTabs') }}
      </h2>
      <div
        class="share-switch shrink-0"
        :data-active="shareTab"
        role="group"
        :aria-label="t('overview.shareTabs')"
      >
        <button
          type="button"
          class="share-switch-btn"
          data-testid="overview-share-tab-model"
          :aria-pressed="shareTab === 'model'"
          @click="shareTab = 'model'"
        >
          {{ t('overview.byModel') }}
        </button>
        <button
          type="button"
          class="share-switch-btn"
          data-testid="overview-share-tab-channel"
          :aria-pressed="shareTab === 'channel'"
          @click="shareTab = 'channel'"
        >
          {{ t('overview.byChannel') }}
        </button>
      </div>
    </div>
    <div class="card-body overview-panel-body seed-scrollbar">
      <ul v-if="loading" class="space-y-4" role="status" :aria-label="t('common.loading')">
        <li
          v-for="row in SHARE_LIST_LIMIT"
          :key="'share-skeleton-' + row"
          class="skeleton-stagger space-y-1.5"
          :style="{ '--skeleton-index': row - 1 }"
        >
          <div class="flex items-baseline justify-between gap-3">
            <SkeletonBlock height="h-4" width="w-28" />
            <SkeletonBlock height="h-3" width="w-12" />
          </div>
          <SkeletonBlock height="h-1.5" width="w-full" rounded="rounded-full" />
          <SkeletonBlock height="h-3" width="w-24" />
        </li>
      </ul>
      <ul v-else-if="ranked.length > 0" class="space-y-4">
        <li
          v-for="item in ranked"
          :key="item.name"
          :data-testid="itemTestId"
          v-bind="nameBinding(item.name)"
          :data-request-count="String(item.requestCount)"
          :data-cost-usd-micros="String(item.costUsdMicros)"
        >
          <div class="flex items-baseline justify-between gap-3 text-sm">
            <span class="min-w-0 truncate font-medium">{{ item.name }}</span>
            <span class="text-fg-muted shrink-0 font-mono text-xs">
              {{ formatUsdMicros(item.costUsdMicros) }}
            </span>
          </div>
          <div
            class="mt-1.5 h-1.5 overflow-hidden rounded-full bg-[var(--seed-surface-alt)]"
            role="meter"
            :aria-valuenow="Math.round(shareRatio(item) * 1000) / 10"
            aria-valuemin="0"
            aria-valuemax="100"
            :aria-label="item.name"
          >
            <div
              class="h-full rounded-full bg-[var(--seed-primary)]"
              :style="{ width: `${Math.min(100, shareRatio(item) * 100)}%` }"
            />
          </div>
          <p class="text-fg-subtle mt-1 font-mono text-xs">
            {{ formatCount(item.requestCount, locale) }}
            {{ t('overview.shareRequests') }}
            <span aria-hidden="true"> · </span>
            {{ t('overview.costShare', { percent: formatPercent(shareRatio(item), locale) }) }}
          </p>
        </li>
      </ul>
      <EmptyState v-else :title="t('common.emptyList')" />
    </div>
  </div>
</template>
