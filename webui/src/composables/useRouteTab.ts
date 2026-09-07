import { computed, watch, type WritableComputedRef } from 'vue';
import { useNavigate, useSearch } from '@tanstack/vue-router';
import type { FileRouteTypes } from '@/routeTree.gen';

type RoutePath = FileRouteTypes['fullPaths'];

/**
 * 页面级 tab 走 URL search param：刷新/分享/跨设备天然保留，replace 写回不入历史栈，
 * 后退回到上一个页面而不是上一个 tab。参数值只在「仍属于合法 tab 集合」时生效，
 * 否则回落到默认 tab，这样角色能力收窄或参数拼错时不会渲染出空内容。
 *
 * 用法：路由 `validateSearch` 暴露同名可选字段，页面传入当前合法 tab 值列表。
 * 合法集合是响应式的：会话 hydrate 或能力变化导致当前 tab 被裁掉时自动回落。
 */
export function useRouteTab<T extends string>(options: {
  from: RoutePath;
  param: string;
  allowed: () => readonly T[];
  fallback: T;
}): WritableComputedRef<T> {
  const { from, param, allowed, fallback } = options;
  const routeSearch = useSearch({ from });
  const navigate = useNavigate({ from });

  const allowedValues = computed(allowed);

  const active = computed<T>(() => {
    const raw = (routeSearch.value as Record<string, unknown>)[param];
    if (typeof raw === 'string' && allowedValues.value.includes(raw as T)) {
      return raw as T;
    }
    return fallback;
  });

  watch(allowedValues, (values) => {
    if (values.length > 0 && !values.includes(active.value)) {
      setTab(values[0] as T);
    }
  });

  function setTab(next: T) {
    if (next === active.value) return;
    void navigate({
      search: (prev: Record<string, unknown>) => ({ ...prev, [param]: next }),
      replace: true,
    });
  }

  return computed<T>({
    get: () => active.value,
    set: setTab,
  });
}
