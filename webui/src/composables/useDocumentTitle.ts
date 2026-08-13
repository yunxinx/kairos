import { watch } from 'vue';
import { useRouterState } from '@tanstack/vue-router';
import { useI18n } from 'vue-i18n';
import { resolveRouteTitleKey, syncDocumentTitle } from '@/lib/document-title';

/** 随路由与语言切换同步浏览器标签页标题。 */
export function useDocumentTitle(): void {
  const { locale } = useI18n();
  const matches = useRouterState({ select: (state) => state.matches });

  function applyTitle(): void {
    syncDocumentTitle(resolveRouteTitleKey(matches.value));
  }

  watch(matches, applyTitle, { immediate: true });
  watch(locale, applyTitle);
}
