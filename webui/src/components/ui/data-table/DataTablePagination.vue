<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import UiSelect, { type UiSelectOption } from '@/components/ui/UiSelect.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import { getPageNumbers } from '@/lib/page-numbers';

const props = defineProps<{
  totalPages: number;
  summary: string;
  pageSizeId: string;
  pageSizeOptions: UiSelectOption[];
  canPrevious: boolean;
  canNext: boolean;
  summaryTestId?: string;
  previousTestId?: string;
  nextTestId?: string;
}>();

const page = defineModel<number>('page', { required: true });
const pageSize = defineModel<string>('pageSize', { required: true });

const { t } = useI18n();

const pageNumbers = computed(() => getPageNumbers(page.value, props.totalPages));

function goToPage(nextPage: number | '...') {
  if (
    nextPage === '...' ||
    nextPage < 1 ||
    nextPage > props.totalPages ||
    nextPage === page.value
  ) {
    return;
  }
  page.value = nextPage;
}
</script>

<template>
  <div
    data-slot="data-table-pagination"
    class="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 px-2"
  >
    <div class="flex min-w-0 flex-1 items-center gap-3">
      <label
        class="text-fg-muted inline-flex shrink-0 items-center gap-2 text-sm whitespace-nowrap"
        :for="pageSizeId"
      >
        <span class="shrink-0">{{ t('common.rowsPerPage') }}</span>
        <span class="page-size-select inline-block shrink-0">
          <UiSelect
            :id="pageSizeId"
            v-model="pageSize"
            position="popper"
            side="top"
            :options="pageSizeOptions"
          />
        </span>
      </label>
      <span
        class="text-fg-muted min-w-0 truncate text-sm"
        :data-testid="summaryTestId"
        :title="summary"
        >{{ summary }}</span
      >
    </div>
    <div class="flex shrink-0 items-center gap-2">
      <div class="inline-flex items-center gap-1">
        <button
          type="button"
          class="btn btn-icon hidden md:inline-flex"
          :aria-label="t('common.firstPage')"
          :disabled="!canPrevious"
          @click="goToPage(1)"
        >
          <UiIcon name="chevrons-left" :size="16" />
        </button>
        <button
          type="button"
          class="btn btn-icon"
          :data-testid="previousTestId"
          :aria-label="t('common.previousPage')"
          :disabled="!canPrevious"
          @click="goToPage(page - 1)"
        >
          <UiIcon name="chevron-left" :size="16" />
        </button>
        <template v-for="(item, index) in pageNumbers" :key="`${item}-${index}`">
          <span v-if="item === '...'" class="text-fg-muted px-1 text-sm">…</span>
          <button
            v-else
            type="button"
            class="btn h-8 min-w-8 px-2"
            :class="item === page ? 'btn-primary' : ''"
            :aria-label="t('common.pageStatus', { page: item, totalPages })"
            :aria-current="item === page ? 'page' : undefined"
            @click="goToPage(item)"
          >
            {{ item }}
          </button>
        </template>
        <button
          type="button"
          class="btn btn-icon"
          :data-testid="nextTestId"
          :aria-label="t('common.nextPage')"
          :disabled="!canNext"
          @click="goToPage(page + 1)"
        >
          <UiIcon name="chevron-right" :size="16" />
        </button>
        <button
          type="button"
          class="btn btn-icon hidden md:inline-flex"
          :aria-label="t('common.lastPage')"
          :disabled="!canNext"
          @click="goToPage(totalPages)"
        >
          <UiIcon name="chevrons-right" :size="16" />
        </button>
      </div>
    </div>
  </div>
</template>
