<script setup lang="ts">
// 工具栏分面筛选：触发器对齐渠道「状态筛选」，浮层内搜索对齐 shadcn-admin CommandInput。
import { computed, ref, useId } from 'vue';
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'reka-ui';
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';

export interface FacetedFilterOption {
  value: string;
  label: string;
  count?: number;
}

const props = defineProps<{
  title: string;
  options: FacetedFilterOption[];
  testId?: string;
}>();

const selected = defineModel<string[]>({ required: true });
const { t } = useI18n();
const query = ref('');
const uid = useId();
const searchId = `${props.testId ?? 'faceted-filter'}-search-${uid}`;

const selectedSet = computed(() => new Set(selected.value));

const visibleOptions = computed(() => {
  const q = query.value.trim().toLowerCase();
  if (!q) return props.options;
  return props.options.filter(
    (option) => option.label.toLowerCase().includes(q) || option.value.toLowerCase().includes(q),
  );
});

const selectedLabels = computed(() =>
  props.options
    .filter((option) => selectedSet.value.has(option.value))
    .map((option) => option.label),
);

function toggle(value: string) {
  const next = new Set(selected.value);
  if (next.has(value)) next.delete(value);
  else next.add(value);
  selected.value = [...next];
}

function clear() {
  selected.value = [];
  query.value = '';
}
</script>

<template>
  <PopoverRoot>
    <PopoverTrigger class="filter-btn" :data-testid="testId" :aria-label="title">
      <template v-if="selectedLabels.length === 0">
        <UiIcon name="plus-circle" :size="14" />
        {{ title }}
      </template>
      <template v-else>
        <UiIcon name="plus-circle" :size="14" />
        {{ title }}
        <span class="faceted-filter-sep" aria-hidden="true" />
        <template v-if="selectedLabels.length <= 2">
          <span
            v-for="label in selectedLabels"
            :key="label"
            class="badge badge-neutral rounded-sm px-1 font-normal"
          >
            {{ label }}
          </span>
        </template>
        <span v-else class="badge badge-neutral rounded-sm px-1 font-normal">
          {{ t('common.selectedCount', { count: selectedLabels.length }) }}
        </span>
      </template>
    </PopoverTrigger>
    <PopoverPortal>
      <PopoverContent align="start" :side-offset="4" class="faceted-filter-menu">
        <div class="faceted-filter-search">
          <UiIcon name="search" :size="14" class="text-fg-subtle shrink-0" />
          <input
            :id="searchId"
            v-model="query"
            type="text"
            class="faceted-filter-search-field"
            :placeholder="title"
            :aria-label="title"
            :data-testid="testId ? `${testId}-search` : undefined"
          />
        </div>
        <div class="seed-scrollbar max-h-64 overflow-y-auto p-1">
          <p v-if="visibleOptions.length === 0" class="text-fg-muted px-2 py-4 text-center text-sm">
            {{ t('common.filterEmpty') }}
          </p>
          <button
            v-for="option in visibleOptions"
            :key="option.value"
            type="button"
            class="sync-filter-option"
            :data-testid="testId ? `${testId}-option` : undefined"
            :data-value="option.value"
            @click="toggle(option.value)"
          >
            <span
              class="sync-filter-box"
              :data-active="String(selectedSet.has(option.value))"
              aria-hidden="true"
            >
              <UiIcon name="check" :size="10" />
            </span>
            <span class="min-w-0 flex-1 truncate">{{ option.label }}</span>
            <span v-if="option.count !== undefined" class="sync-filter-count">{{
              option.count
            }}</span>
          </button>
          <template v-if="selected.length > 0">
            <div class="my-1 border-t border-[var(--seed-border)]" aria-hidden="true" />
            <button
              type="button"
              class="sync-filter-option justify-center"
              :data-testid="testId ? `${testId}-clear` : undefined"
              @click="clear"
            >
              {{ t('common.clearFilters') }}
            </button>
          </template>
        </div>
      </PopoverContent>
    </PopoverPortal>
  </PopoverRoot>
</template>
