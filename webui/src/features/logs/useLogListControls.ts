import { computed, onUnmounted, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { LOGS_INITIAL_PAGE, LOGS_INITIAL_PAGE_SIZE } from '@/lib/admin-query-defaults';
import type { DateRange } from '@/lib/date-range';
import { scrollMainToTop } from '@/lib/main-scroll';

const PAGE_SIZE_OPTIONS = [20, 50, 100, 200] as const;
const KEYWORD_DEBOUNCE_MS = 300;

/** 请求日志与系统日志共用的关键字/时间/分页控件。 */
export function useLogListControls() {
  const { t } = useI18n();
  const draftKeyword = ref('');
  const appliedKeyword = ref('');
  const appliedRange = ref<DateRange>({ from: null, to: null });
  const page = ref(LOGS_INITIAL_PAGE);
  const pageSize = ref(LOGS_INITIAL_PAGE_SIZE);
  const appliedFrom = computed(() => appliedRange.value.from);
  const appliedTo = computed(() => appliedRange.value.to);

  const pageSizeModel = computed({
    get: () => String(pageSize.value),
    set: (value: string) => {
      const parsed = Number.parseInt(value, 10);
      if (Number.isNaN(parsed) || parsed === pageSize.value) {
        return;
      }
      pageSize.value = parsed;
      page.value = 1;
    },
  });

  const pageSizeOptions = computed(() =>
    PAGE_SIZE_OPTIONS.map((size) => ({
      value: String(size),
      label: String(size),
    })),
  );

  function resetResults() {
    page.value = 1;
  }

  let keywordTimer: number | undefined;

  function applyKeywordNow() {
    window.clearTimeout(keywordTimer);
    keywordTimer = undefined;
    if (appliedKeyword.value === draftKeyword.value) {
      return;
    }
    appliedKeyword.value = draftKeyword.value;
    resetResults();
  }

  watch(draftKeyword, () => {
    window.clearTimeout(keywordTimer);
    keywordTimer = window.setTimeout(applyKeywordNow, KEYWORD_DEBOUNCE_MS);
  });

  watch(appliedRange, resetResults);
  watch(page, () => {
    scrollMainToTop();
  });

  onUnmounted(() => {
    window.clearTimeout(keywordTimer);
  });

  function clearBaseFilters() {
    window.clearTimeout(keywordTimer);
    keywordTimer = undefined;
    draftKeyword.value = '';
    appliedKeyword.value = '';
    appliedRange.value = { from: null, to: null };
    resetResults();
  }

  function pagination(total: number) {
    const totalPages = Math.max(1, Math.ceil(total / pageSize.value));
    return {
      totalPages,
      canGoPrevious: page.value > 1,
      canGoNext: page.value < totalPages && total > 0,
      summary: t('logs.paginationSummary', {
        page: page.value,
        totalPages,
        total,
      }),
    };
  }

  return {
    draftKeyword,
    appliedKeyword,
    appliedRange,
    appliedFrom,
    appliedTo,
    page,
    pageSize,
    pageSizeModel,
    pageSizeOptions,
    applyKeywordNow,
    resetResults,
    clearBaseFilters,
    pagination,
  };
}
