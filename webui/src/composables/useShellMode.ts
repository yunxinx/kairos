import { computed } from 'vue';
import { useRouterState } from '@tanstack/vue-router';
import { hasAdminKey } from '@/lib/session';

/** 根据当前路由与是否持有 admin key 切换 App shell chrome。 */
export function useShellMode() {
  const pathname = useRouterState({ select: (state) => state.location.pathname });

  const showAdminNav = computed(() => hasAdminKey() && pathname.value !== '/login');
  const showMarketingChrome = computed(() => !showAdminNav.value);
  const suppressPageContent = computed(() => false);
  const showPublicFooter = computed(() => showMarketingChrome.value);

  return {
    showAdminNav,
    showMarketingChrome,
    suppressPageContent,
    showPublicFooter,
  };
}
