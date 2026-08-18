<script setup lang="ts">
// 浮窗：就近锚定触发点打开、标题栏整行可拖拽（按钮区域除外）、非模态；由调用方 v-if 挂载，随路由卸载。
import { onMounted, onScopeDispose, onUnmounted, reactive, ref, useId, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    /** 标题栏文案，同时作为对话框可访问名称。 */
    title: string;
    /** 打开时的视口锚点；缺省居中。 */
    anchor?: FloatingWindowAnchor | null;
    /** 字段较多的表单用更宽面板。 */
    wide?: boolean;
    /** 多列表格预览用更宽面板（对照目录填价）。 */
    extraWide?: boolean;
    /** 窗口栈层叠序号，叠加在 --z-window 之上；越大越靠前。 */
    stackOrder?: number;
    /** 初始位置级联偏移序号，避免多个窗口完全重叠。 */
    cascade?: number;
    /** 淘汰被阻止时的短暂提示动画。 */
    attention?: boolean;
    /** 是否最前窗口；仅最前窗口响应 Esc 关闭。 */
    topmost?: boolean;
    /** 关闭按钮的可访问名称；缺省为「关闭」。子视图可改成「不保存并返回」等。 */
    closeAriaLabel?: string | null;
  }>(),
  {
    anchor: null,
    wide: false,
    extraWide: false,
    stackOrder: 0,
    cascade: 0,
    attention: false,
    topmost: true,
    closeAriaLabel: null,
  },
);

const emit = defineEmits<{
  close: [];
}>();

const { t } = useI18n();
const titleId = useId();
const windowEl = ref<HTMLElement | null>(null);
const position = reactive({ left: 0, top: 0 });
const dragging = reactive({ active: false, offsetX: 0, offsetY: 0 });
/** 内容视图切换时锁定窗口尺寸，避免两个视图内容高度不同造成窗口跳变。 */
const lockedSize = ref<{ width: number; height: number } | null>(null);

let restoreFocusEl: HTMLElement | null = null;

const VIEWPORT_MARGIN = 8;

function clampLeft(left: number, width: number): number {
  const max = Math.max(VIEWPORT_MARGIN, window.innerWidth - width - VIEWPORT_MARGIN);
  return Math.min(Math.max(left, VIEWPORT_MARGIN), max);
}

function clampTop(top: number, height: number): number {
  const max = Math.max(VIEWPORT_MARGIN, window.innerHeight - height - VIEWPORT_MARGIN);
  return Math.min(Math.max(top, VIEWPORT_MARGIN), max);
}

/** 内容变高或视口缩小后，把窗口重新夹进可视区域（例如从底部批量条打开后预览表展开）。 */
function keepInViewport() {
  const el = windowEl.value;
  if (!el) return;
  position.left = clampLeft(position.left, el.offsetWidth);
  position.top = clampTop(position.top, el.offsetHeight);
}

/** 以当前渲染尺寸锁定窗口，供内容视图切换时保持大小；`unlockSize` 恢复自适应。 */
function lockSize() {
  const el = windowEl.value;
  if (!el) return;
  lockedSize.value = { width: el.offsetWidth, height: el.offsetHeight };
}

function unlockSize() {
  lockedSize.value = null;
}

defineExpose({ lockSize, unlockSize });

function startDrag(event: PointerEvent): void {
  // 标题栏整行可拖拽，但点在其中的按钮（关闭按钮、页签开关等）上不发起拖拽。
  if (event.button !== 0 || (event.target as HTMLElement).closest('button')) return;
  (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  dragging.active = true;
  dragging.offsetX = event.clientX - position.left;
  dragging.offsetY = event.clientY - position.top;
}

function onDrag(event: PointerEvent): void {
  if (!dragging.active) return;
  const el = windowEl.value;
  if (!el) return;
  position.left = clampLeft(event.clientX - dragging.offsetX, el.offsetWidth);
  position.top = clampTop(event.clientY - dragging.offsetY, el.offsetHeight);
}

function endDrag(event: PointerEvent): void {
  if (!dragging.active) return;
  dragging.active = false;
  (event.currentTarget as HTMLElement).releasePointerCapture(event.pointerId);
}

function isEditable(el: Element): el is HTMLElement {
  return (
    el instanceof HTMLInputElement ||
    el instanceof HTMLTextAreaElement ||
    el instanceof HTMLSelectElement ||
    (el instanceof HTMLElement && el.isContentEditable)
  );
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Escape') return;
  const active = document.activeElement;
  if (active && active !== document.body && isEditable(active)) {
    if (windowEl.value?.contains(active)) {
      // 窗内输入框：Esc 只取消焦点，等同点输入框外侧，不关窗口。
      event.preventDefault();
      active.blur();
    }
    // 窗外可编辑控件：不拦截，避免误关窗口。
    return;
  }
  event.preventDefault();
  emit('close');
}

watch(
  () => props.topmost,
  (isTopmost) => {
    if (isTopmost) document.addEventListener('keydown', onKeydown);
    else document.removeEventListener('keydown', onKeydown);
  },
  { immediate: true },
);

onScopeDispose(() => {
  document.removeEventListener('keydown', onKeydown);
});

let resizeObserver: ResizeObserver | undefined;

onMounted(() => {
  restoreFocusEl = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const el = windowEl.value;
  if (el) {
    const cascadeOffset = props.cascade * 24;
    if (props.anchor) {
      position.left = clampLeft(props.anchor.x + cascadeOffset, el.offsetWidth);
      position.top = clampTop(props.anchor.y + VIEWPORT_MARGIN + cascadeOffset, el.offsetHeight);
    } else {
      position.left = clampLeft(
        (window.innerWidth - el.offsetWidth) / 2 + cascadeOffset,
        el.offsetWidth,
      );
      position.top = clampTop(
        (window.innerHeight - el.offsetHeight) / 2 + cascadeOffset,
        el.offsetHeight,
      );
    }
    resizeObserver = new ResizeObserver(() => keepInViewport());
    resizeObserver.observe(el);
  }
  window.addEventListener('resize', keepInViewport);
});

onUnmounted(() => {
  resizeObserver?.disconnect();
  resizeObserver = undefined;
  window.removeEventListener('resize', keepInViewport);
  const target = restoreFocusEl;
  restoreFocusEl = null;
  if (!target || !target.isConnected || !document.hasFocus()) return;
  const active = document.activeElement;
  if (active === document.body || active === document.documentElement) {
    target.focus({ preventScroll: true });
  }
});
</script>

<template>
  <div
    ref="windowEl"
    role="dialog"
    :aria-labelledby="titleId"
    class="floating-window card flex max-h-[calc(100vh-1rem)] max-w-[calc(100vw-1rem)] flex-col"
    :class="[
      extraWide ? 'w-[56rem]' : wide ? 'w-[34rem]' : 'w-[28rem]',
      attention && 'floating-window-attention',
    ]"
    :style="{
      left: `${position.left}px`,
      top: `${position.top}px`,
      zIndex: `calc(var(--z-window) + ${stackOrder})`,
      width: lockedSize ? `${lockedSize.width}px` : undefined,
      height: lockedSize ? `${lockedSize.height}px` : undefined,
    }"
  >
    <div
      class="card-header shrink-0 cursor-move touch-none select-none"
      @pointerdown="startDrag"
      @pointermove="onDrag"
      @pointerup="endDrag"
      @pointercancel="endDrag"
    >
      <div class="flex min-w-0 flex-1 items-center gap-2">
        <h2 :id="titleId" class="min-w-0 truncate font-serif text-base font-semibold">
          {{ title }}
        </h2>
        <slot name="header-extra" />
      </div>
      <button
        type="button"
        class="btn btn-ghost btn-sm"
        :aria-label="closeAriaLabel ?? t('common.close')"
        @click="emit('close')"
      >
        <UiIcon name="close" :size="16" />
      </button>
    </div>
    <div
      class="min-h-0 flex-1"
      :class="lockedSize ? 'flex flex-col overflow-hidden' : 'overflow-y-auto'"
    >
      <slot />
    </div>
  </div>
</template>
