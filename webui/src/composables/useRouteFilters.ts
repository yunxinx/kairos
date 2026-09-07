import { computed, onUnmounted, ref, watch, type WritableComputedRef } from 'vue';
import { useNavigate, useSearch } from '@tanstack/vue-router';
import type { FileRouteTypes } from '@/routeTree.gen';

type RoutePath = FileRouteTypes['fullPaths'];

const SEARCH_WRITE_DEBOUNCE_MS = 300;

/** 从 search 对象里读出字符串参数；不存在或非字符串时给 undefined。 */
function readStringParam(search: Record<string, unknown>, key: string): string | undefined {
  const raw = search[key];
  return typeof raw === 'string' ? raw : undefined;
}

/** 从 search 对象里读出字符串数组参数；逗号分隔，非法项丢弃。 */
function readListParam(search: Record<string, unknown>, key: string): string[] {
  const raw = search[key];
  if (typeof raw !== 'string') return [];
  return raw
    .split(',')
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
}

/** 值为空时从 URL 摘掉参数，保持地址栏干净（默认值不落 URL）。 */
function pickParam(value: string): string | undefined {
  return value.length > 0 ? value : undefined;
}

function pickListParam(values: string[]): string | undefined {
  return values.length > 0 ? values.join(',') : undefined;
}

/**
 * 工具栏筛选走 URL search param（replace 写回）：可分享、刷新保留，与 tab 持久化
 * 同一套机制。一个 composable 同时接管页面上的搜索词与各筛选 chip：
 *
 * - `searchParam`：单字符串参数，双向绑定。
 * - `listParam`：多值参数，逗号分隔（`?role=admin,root`）。空数组不落 URL。
 * - `debouncedSearchParam`：搜索词防抖写回（300ms），输入停顿或回车时落 URL，
 *   中间态不污染地址栏；URL 外部变化（返回链接、清筛选按钮）时草稿跟随。
 *
 * 路由侧需要 `validateSearch` 把这些参数声明为 `string | undefined`。
 */
export function useRouteFilters(from: RoutePath, params: readonly string[]) {
  const routeSearch = useSearch({ from });
  const navigate = useNavigate({ from });

  function searchRecord(): Record<string, unknown> {
    return routeSearch.value;
  }

  function write(patch: Record<string, string | undefined>) {
    const hasChange = params.some((key) => {
      const next = patch[key];
      // 空值（undefined/空串）代表「把参数摘掉」：与 URL 上不存在同义。
      const nextValue = next === undefined || next === '' ? undefined : next;
      const currentValue = readStringParam(searchRecord(), key);
      return nextValue !== currentValue;
    });
    if (!hasChange) return;
    void navigate({
      search: (prev: Record<string, unknown>) => ({ ...prev, ...patch }),
      replace: true,
    });
  }

  function searchParam(param: string): WritableComputedRef<string> {
    return computed<string>({
      get: () => readStringParam(searchRecord(), param) ?? '',
      set: (next) => {
        write({ [param]: pickParam(next) });
      },
    });
  }

  function listParam(param: string): WritableComputedRef<string[]> {
    return computed<string[]>({
      get: () => readListParam(searchRecord(), param),
      set: (next) => {
        write({ [param]: pickListParam(next) });
      },
    });
  }

  /**
   * 搜索词防抖写回：输入停顿或回车时落 URL，中间态不污染地址栏。
   * - `draft`：绑定输入框与客户端过滤，逐键即时生效；
   * - `applied`：URL 落定值，只在防抖/回车提交后变化——查询 queryKey 应绑定
   *   这一个，避免逐键触发请求。
   * URL 外部变化（返回链接、清筛选按钮）时草稿跟随——但只在没有待写内容时跟随：
   * 否则「清筛选的 navigate 异步回来」会覆盖掉用户刚敲进来的新词。
   */
  function debouncedSearchParam(param: string) {
    const applied = searchParam(param);
    const draft = ref(applied.value);
    let timer: ReturnType<typeof setTimeout> | undefined;

    function pending(): boolean {
      return timer !== undefined;
    }

    watch(applied, (next) => {
      if (next !== draft.value && !pending()) draft.value = next;
    });

    watch(draft, (next) => {
      if (next === applied.value) {
        window.clearTimeout(timer);
        timer = undefined;
        return;
      }
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        timer = undefined;
        applied.value = next;
      }, SEARCH_WRITE_DEBOUNCE_MS);
    });

    function commitNow() {
      window.clearTimeout(timer);
      timer = undefined;
      applied.value = draft.value;
    }

    onUnmounted(() => {
      window.clearTimeout(timer);
    });

    return { draft, applied, commitNow };
  }

  return { searchParam, listParam, debouncedSearchParam };
}
