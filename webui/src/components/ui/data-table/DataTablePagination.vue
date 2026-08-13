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
    class="flex flex-wrap items-center justify-between gap-3 overflow-hidden px-2"
  >
    <div class="flex flex-wrap items-center gap-3">
      <label class="text-fg-muted inline-flex items-center gap-2 text-sm" :for="pageSizeId">
        <span>{{ t('common.rowsPerPage') }}</span>
        <UiSelect
          :id="pageSizeId"
          v-model="pageSize"
          class="min-w-[4.5rem]"
          :options="pageSizeOptions"
        />
      </label>
      <span class="text-fg-muted text-sm" :data-testid="summaryTestId">{{ summary }}</span>
    </div>
    <div class="flex items-center gap-2">
      <span class="text-fg-muted hidden text-sm font-medium sm:inline">
        {{ t('common.pageStatus', { page, totalPages }) }}
      </span>
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
