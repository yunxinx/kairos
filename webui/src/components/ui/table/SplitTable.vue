<script setup lang="ts">
// 表头在滚动容器外：垂直滚动条只出现在表体。横向滚动由表体驱动并同步到表头。
// 经典滚动条会吃掉表体内容宽度，表头用 paddingInlineEnd 对齐列；overlay 滚动条测得 0。
// 根节点必须有明确高度（h-full / h-96 / bounded-table-*）：min-h-0 + overflow-auto 在高度为 auto 时会把表体压成 0。
// tbody 先铺一列宽占位行，避免分组行 colspan 当首行时 table-layout:fixed 把后续行列宽打乱。
import { onUnmounted, ref, useTemplateRef, watch } from 'vue';
import { cn } from '@/lib/cn';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';

const TABLE_CLASS = 'w-full table-fixed caption-bottom overflow-hidden text-sm';

const props = withDefaults(
  defineProps<{
    columns: { width: string }[];
    class?: string;
  }>(),
  {
    class: '',
  },
);

const headerEl = useTemplateRef<HTMLElement>('headerEl');
const bodyEl = useTemplateRef<HTMLElement>('bodyEl');
const verticalGutter = ref(0);

let resizeObserver: ResizeObserver | undefined;

function syncHeaderScroll() {
  const header = headerEl.value;
  const body = bodyEl.value;
  if (header === null || body === null) return;
  if (header.scrollLeft !== body.scrollLeft) {
    header.scrollLeft = body.scrollLeft;
  }
}

function syncScrollbarSize() {
  const body = bodyEl.value;
  if (body === null) {
    verticalGutter.value = 0;
    return;
  }
  const next = Math.max(0, body.offsetWidth - body.clientWidth);
  if (next !== verticalGutter.value) {
    verticalGutter.value = next;
  }
}

function bindObserver(el: HTMLElement | null) {
  resizeObserver?.disconnect();
  resizeObserver = undefined;
  if (el === null) {
    verticalGutter.value = 0;
    return;
  }
  resizeObserver = new ResizeObserver(() => {
    syncScrollbarSize();
  });
  resizeObserver.observe(el);
  const content = el.firstElementChild;
  if (content !== null) {
    resizeObserver.observe(content);
  }
  syncScrollbarSize();
}

watch(bodyEl, (el) => bindObserver(el), { immediate: true, flush: 'post' });

onUnmounted(() => {
  resizeObserver?.disconnect();
});

function getScrollElement(): HTMLElement | null {
  return bodyEl.value;
}

defineExpose({ getScrollElement });
</script>

<template>
  <div data-slot="split-table" :class="cn('flex min-h-0 flex-col overflow-hidden', props.class)">
    <div
      ref="headerEl"
      data-slot="split-table-header"
      class="shrink-0 overflow-hidden"
      :style="{ paddingInlineEnd: `${verticalGutter}px` }"
    >
      <table :class="TABLE_CLASS">
        <colgroup>
          <col v-for="(column, index) in columns" :key="index" :style="{ width: column.width }" />
        </colgroup>
        <TableHeader>
          <slot name="header" />
        </TableHeader>
      </table>
    </div>
    <div
      ref="bodyEl"
      data-slot="split-table-body"
      class="seed-scrollbar min-h-0 flex-1 overflow-auto"
      @scroll="syncHeaderScroll"
    >
      <table :class="TABLE_CLASS">
        <colgroup>
          <col v-for="(column, index) in columns" :key="index" :style="{ width: column.width }" />
        </colgroup>
        <TableBody>
          <TableRow class="split-table-sizer" aria-hidden="true">
            <TableCell v-for="index in columns.length" :key="'sizer-' + index" />
          </TableRow>
          <slot />
        </TableBody>
      </table>
    </div>
  </div>
</template>
