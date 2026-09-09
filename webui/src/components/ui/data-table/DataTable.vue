<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, onUpdated, ref, useTemplateRef, watch } from 'vue';
import { cn } from '@/lib/cn';
import { useI18n } from 'vue-i18n';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';

defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    class?: string;
    /** 表体正在加载骨架时标忙，避免读屏把空表当结果。 */
    busy?: boolean;
  }>(),
  {
    class: '',
    busy: false,
  },
);

const { t } = useI18n();

const rootEl = useTemplateRef<HTMLElement>('rootEl');

/** tbody 里是否有真实内容行（骨架行计入内容；空态行标了 data-empty-row，排除）。 */
function hasContentRows(): boolean {
  const rows = rootEl.value?.querySelectorAll(
    "[data-slot='table-body'] > [data-slot='table-row']:not([data-empty-row])",
  );
  return (rows?.length ?? 0) > 0;
}

/**
 * 空表摘掉卡片外壳（DOM 探测：行内容由插槽提供，组件拿不到数据）。
 * 只切换面板 class、不换节点：空↔非空交叉不重挂表格，焦点与滚动位置不丢。
 */
const isEmpty = ref(true);
function syncEmpty(): void {
  isEmpty.value = !hasContentRows();
}

/**
 * 骨架→数据的行入场：真实行按序淡入上浮，自上而下铺开。
 * 每个组件实例只播一次（hasPlayedEntrance）：翻页、筛选、改查询条件引发的
 * busy 反复翻转都不重播；挂载即有缓存数据（busy 恒 false）的路径也不播。
 * 空表判定放到 nextTick 之后：pre-flush watch 读到的是骨架行还在的旧 DOM。
 */
let wasBusy = props.busy;
let hasPlayedEntrance = false;
const justLoaded = ref(false);
let justLoadedTimer: ReturnType<typeof setTimeout> | undefined;

function stampRowIndexes(): void {
  // 动画延迟按 DOM 行序注入 --row-index；骨架行已随 v-else 卸载，此处只标数据行。
  const rows = rootEl.value?.querySelectorAll<HTMLElement>(
    "[data-slot='table-body'] > [data-slot='table-row']:not([data-empty-row])",
  );
  rows?.forEach((row, index) => {
    row.style.setProperty('--row-index', String(index));
  });
}

watch(
  () => props.busy,
  (busy) => {
    if (wasBusy && !busy && !hasPlayedEntrance) {
      void nextTick(() => {
        // 空表不播入场动画：没有行可入，播了只是空文案闪一下。
        if (!hasContentRows()) return;
        hasPlayedEntrance = true;
        justLoaded.value = true;
        stampRowIndexes();
        clearTimeout(justLoadedTimer);
        // 700ms ≈ 160ms 动画 + 20 行 × 24ms 递增封顶（640ms），留余量后摘 class。
        justLoadedTimer = setTimeout(() => {
          justLoaded.value = false;
        }, 700);
      });
    }
    wasBusy = busy;
  },
);

onMounted(syncEmpty);
// 窗口期内后挂载的行（翻页等）也要盖上 --row-index，才有按序延迟。
onUpdated(() => {
  syncEmpty();
  if (justLoaded.value) stampRowIndexes();
});

onBeforeUnmount(() => clearTimeout(justLoadedTimer));
</script>

<template>
  <!--
    单表（thead+tbody 同一 <table>）+ 工具栏在边框外 + 分页在边框下。
    刻意不拆表头/表体、不做虚拟滚动：管理端列表短，分表是列宽错位的主因。
    表格自然撑开高度，垂直滚动由页面（main）承担，表内只处理横向滚动。
    空表时面板外壳透明化（data-table-panel-bare），只留表头与空态文字。
  -->
  <div
    ref="rootEl"
    data-slot="data-table"
    v-bind="$attrs"
    :role="props.busy ? 'status' : undefined"
    :aria-label="props.busy ? t('common.loading') : undefined"
    :class="cn('flex flex-col gap-4', justLoaded && 'data-table-just-loaded', props.class)"
  >
    <div v-if="$slots.toolbar" data-slot="data-table-toolbar-slot" class="shrink-0">
      <slot name="toolbar" />
    </div>
    <DataTablePanel :class="isEmpty ? 'data-table-panel-bare' : ''">
      <div data-slot="data-table-scroll" class="seed-scrollbar relative w-full overflow-x-auto">
        <table
          data-slot="table"
          class="w-full caption-bottom text-sm"
          :aria-busy="props.busy ? 'true' : undefined"
        >
          <slot />
        </table>
      </div>
    </DataTablePanel>
    <div v-if="$slots.pagination" data-slot="data-table-pagination-slot" class="shrink-0">
      <slot name="pagination" />
    </div>
  </div>
</template>
