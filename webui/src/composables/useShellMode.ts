import { computed } from 'vue';
import { useRouterState } from '@tanstack/vue-router';
import { hasSession } from '@/lib/session';

/** 根据当前路由与是否持有 admin key 切换 App shell chrome。 */
export function useShellMode() {
  // 用 resolvedLocation（与路由组件树同一批次提交）而非 location（导航一开始就翻转）。
  // 订阅 location 会在「外壳已换装、页面组件还没换」的间隙里拆掉营销页布局，
  // 登录卡在卸载前先被 reflow 到页面顶部——就是那个「表单先上移再消失」的跳变。
  const pathname = useRouterState({
    select: (state) => state.resolvedLocation?.pathname ?? state.location.pathname,
  });

  const isMarketingPath = computed(() => pathname.value === '/' || pathname.value === '/login');
  const showAdminNav = computed(() => hasSession() && !isMarketingPath.value);
  const showMarketingChrome = computed(() => !showAdminNav.value);
  const showPublicFooter = computed(() => showMarketingChrome.value);

  return {
    showAdminNav,
    showMarketingChrome,
    showPublicFooter,
  };
}
