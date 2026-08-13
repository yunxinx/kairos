import { onMounted, onUnmounted, ref, type Ref } from 'vue';
import { getStoredTheme, initTheme, resolveDark } from '@/lib/theme';

/** 挂载时初始化主题，并监听系统配色偏好；卸载时移除 matchMedia 监听器。 */
export function useResolvedDarkTheme(): Ref<boolean> {
  const isDark = ref(false);
  let mediaQuery: MediaQueryList | undefined;
  let onSchemeChange: (() => void) | undefined;

  onMounted(() => {
    isDark.value = initTheme();
    mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
    onSchemeChange = () => {
      isDark.value = resolveDark(getStoredTheme());
    };
    mediaQuery.addEventListener('change', onSchemeChange);
  });

  onUnmounted(() => {
    if (mediaQuery && onSchemeChange) {
      mediaQuery.removeEventListener('change', onSchemeChange);
    }
  });

  return isDark;
}
