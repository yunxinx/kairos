import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRouteFilters } from '@/composables/useRouteFilters';
import { LOGS_INITIAL_PAGE, LOGS_INITIAL_PAGE_SIZE } from '@/lib/admin-query-defaults';
import type { DateRange } from '@/lib/date-range';
import { readValidatedNumber, writePlainNumber } from '@/lib/preferences';
import { scrollMainToTop } from '@/lib/main-scroll';

const PAGE_SIZE_OPTIONS = [20, 50, 100, 200] as const;
const PAGE_SIZE_KEY = 'kairos-logs-page-size';

function readStoredPageSize(): number {
  return readValidatedNumber(PAGE_SIZE_KEY, PAGE_SIZE_OPTIONS, LOGS_INITIAL_PAGE_SIZE);
}

/**
 * 请求日志与系统日志共用的关键字/时间/分页控件。
 * 关键字走 /logs?q=…（replace 写回、300ms 防抖），两个面板共用同一条 URL 状态。
 * page size 属个人偏好，落 localStorage（合法档位校验，页码不持久化）。
 */
export function useLogListControls() {
  const { t } = useI18n();
  const { debouncedSearchParam } = useRouteFilters('/logs', ['q']);
  // 草稿逐键进输入框；applied 是 URL 落定值（防抖/回车后才变），
  // 查询 queryKey 只绑它，保持「输入停顿才发请求」的原有防抖语义。
  const {
    draft: draftKeyword,
    applied: appliedKeyword,
    commitNow: applyKeywordNow,
  } = debouncedSearchParam('q');
  const appliedRange = ref<DateRange>({ from: null, to: null });
  const page = ref(LOGS_INITIAL_PAGE);
  const pageSize = ref(readStoredPageSize());
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
      writePlainNumber(PAGE_SIZE_KEY, parsed);
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

  watch(appliedKeyword, resetResults);
  watch(appliedRange, resetResults);
  watch(page, () => {
    scrollMainToTop();
  });

  function clearBaseFilters() {
    draftKeyword.value = '';
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
