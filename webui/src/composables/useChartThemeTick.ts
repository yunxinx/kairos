import { onMounted, onUnmounted, ref, type Ref } from 'vue';

/** ECharts 浮窗/轴线随 `html.dark` 切换时重读 CSS 变量。 */
export function useChartThemeTick(): Ref<number> {
  const themeTick = ref(0);
  let themeObserver: MutationObserver | undefined;

  onMounted(() => {
    themeObserver = new MutationObserver(() => {
      themeTick.value += 1;
    });
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['class'],
    });
  });

  onUnmounted(() => {
    themeObserver?.disconnect();
  });

  return themeTick;
}
