<script setup lang="ts">
// 表头右缘的列宽拖拽把手：pointerdown 捕获后随指针横向移动，把列宽回调给页面。
// 拖动结束（pointerup）单独回调，页面据此落 localStorage——拖动过程中只改内联样式。
// 基准宽度在每次交互时从所在 th 实测（上一轮拖拽改过的宽度会立即反映在 DOM 上），
// 不经 props 传递：props 的宽值取自渲染期计算，拿到的往往是上一次布局的旧值。
// 双击重置单列；键盘方向键 ±8px 微调。把手在 th 右缘 6px 命中区，
// 叠在 padding 上不占列内容空间。
import { onUnmounted, ref, useTemplateRef } from 'vue';
import { useI18n } from 'vue-i18n';

const KEYBOARD_STEP_PX = 8;

const props = defineProps<{
  columnId: string;
}>();

const emit = defineEmits<{
  /** 拖动中/键盘调整：请求把列宽改为 width。 */
  resize: [id: string, width: number];
  /** 一次调整结束（pointerup / 键盘一步）：落盘时机。 */
  commit: [];
  /** 双击：重置该列。 */
  reset: [id: string];
}>();

const { t } = useI18n();
const handleEl = useTemplateRef<HTMLElement>('handleEl');
const dragging = ref(false);

let pointerId: number | undefined;
let originX = 0;
let originWidth = 0;

/** 当前列的实测渲染宽度；把手不在 th 内时无意义（不会发生）。 */
function measuredWidth(): number {
  const th = handleEl.value?.closest('th');
  return th?.getBoundingClientRect().width ?? 0;
}

function onPointerdown(event: PointerEvent) {
  if (event.button !== 0) return;
  event.preventDefault();
  pointerId = event.pointerId;
  originX = event.clientX;
  originWidth = measuredWidth();
  dragging.value = true;
  window.addEventListener('pointermove', onPointermove);
  window.addEventListener('pointerup', onPointerup);
  window.addEventListener('pointercancel', onPointerup);
}

function onPointermove(event: PointerEvent) {
  if (event.pointerId !== pointerId) return;
  emit('resize', props.columnId, originWidth + event.clientX - originX);
}

function onPointerup(event: PointerEvent) {
  if (event.pointerId !== pointerId) return;
  finishDrag();
}

function finishDrag() {
  window.removeEventListener('pointermove', onPointermove);
  window.removeEventListener('pointerup', onPointerup);
  window.removeEventListener('pointercancel', onPointerup);
  pointerId = undefined;
  dragging.value = false;
  emit('commit');
}

onUnmounted(finishDrag);

function onKeydown(event: KeyboardEvent) {
  if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
  event.preventDefault();
  const delta = event.key === 'ArrowLeft' ? -KEYBOARD_STEP_PX : KEYBOARD_STEP_PX;
  emit('resize', props.columnId, measuredWidth() + delta);
  emit('commit');
}

function onDblclick() {
  emit('reset', props.columnId);
}
</script>

<template>
  <!-- eslint-disable-next-line vuejs-accessibility/no-static-element-interactions -- 拖拽把手是可聚焦 separator（tabindex + aria-valuenow），pointerdown 是其指针交互入口，规则不认这个模式。 -->
  <span
    ref="handleEl"
    role="separator"
    tabindex="0"
    class="column-resize-handle"
    :class="{ 'column-resize-handle-active': dragging }"
    :aria-label="t('common.resizeColumn')"
    :aria-orientation="'vertical'"
    :aria-valuenow="Math.round(measuredWidth())"
    :data-testid="`column-resize-${columnId}`"
    @pointerdown="onPointerdown"
    @keydown="onKeydown"
    @dblclick="onDblclick"
  />
</template>
