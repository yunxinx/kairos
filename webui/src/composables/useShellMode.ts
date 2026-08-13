import { computed } from 'vue';
import { useRouterState } from '@tanstack/vue-router';
import { hasAdminKey } from '@/lib/session';

/** 根据当前路由与是否持有 admin key 切换 App shell chrome。 */
export function useShellMode() {
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const matches = useRouterState({ select: (state) => state.matches });

  const isMarketingPath = computed(() => pathname.value === '/' || pathname.value === '/login');
  const showAdminNav = computed(() => hasAdminKey() && !isMarketingPath.value);
  const showMarketingChrome = computed(() => !showAdminNav.value);
  const suppressPageContent = computed(() => false);
  const showPublicFooter = computed(() => showMarketingChrome.value);
  /** 表格页填满视口并在表内滚动；其余页面由 main 承担滚轮。 */
  const fillViewport = computed(() =>
    matches.value.some((match) => Boolean(match.staticData.fillViewport)),
  );

  return {
    showAdminNav,
    showMarketingChrome,
    suppressPageContent,
    showPublicFooter,
    fillViewport,
  };
}
