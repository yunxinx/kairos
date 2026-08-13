<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue';
import { useI18n } from 'vue-i18n';

withDefaults(
  defineProps<{
    /** 对应标题元素的 id，用于 aria-labelledby。 */
    labelledBy?: string | undefined;
    /** 无可见标题时提供对话框名称。 */
    ariaLabel?: string | undefined;
    /** 渠道等字段较多的表单用更宽面板。 */
    wide?: boolean;
  }>(),
  { labelledBy: undefined, ariaLabel: undefined, wide: false },
);

const emit = defineEmits<{
  close: [];
}>();

const { t } = useI18n();

const dialogEl = ref<HTMLDialogElement | null>(null);
let restoreFocusEl: HTMLElement | null = null;
let usingNativeModal = false;

function isDialogOpen(dialog: HTMLDialogElement): boolean {
  return dialog.open || dialog.hasAttribute('open');
}

function openDialog(dialog: HTMLDialogElement): void {
  if (typeof dialog.showModal === 'function') {
    dialog.showModal();
    usingNativeModal = true;
    return;
  }
  // jsdom 等环境无 showModal，退化为 open 属性 + Esc 监听。
  dialog.setAttribute('open', '');
  usingNativeModal = false;
}

function closeDialog(dialog: HTMLDialogElement): void {
  if (usingNativeModal && dialog.open) {
    dialog.close();
    return;
  }
  dialog.removeAttribute('open');
}

function canRestoreFocus(target: HTMLElement): boolean {
  if (!target.isConnected || !document.hasFocus()) {
    return false;
  }

  const dialog = dialogEl.value;
  const activeElement = document.activeElement;
  if (!dialog || !activeElement) {
    return true;
  }

  return (
    activeElement === document.body ||
    activeElement === document.documentElement ||
    activeElement === dialog ||
    dialog.contains(activeElement)
  );
}

function restoreFocus(): void {
  const target = restoreFocusEl;
  restoreFocusEl = null;
  if (!target || !canRestoreFocus(target)) {
    return;
  }
  target.focus({ preventScroll: true });
}

function onEscapeKey(event: KeyboardEvent): void {
  if (event.key !== 'Escape') {
    return;
  }
  event.preventDefault();
  requestClose();
}

onMounted(() => {
  const dialog = dialogEl.value;
  if (!dialog) {
    return;
  }
  restoreFocusEl = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  openDialog(dialog);
  if (!usingNativeModal) {
    document.addEventListener('keydown', onEscapeKey);
  }
});

onUnmounted(() => {
  const dialog = dialogEl.value;
  if (dialog && isDialogOpen(dialog)) {
    closeDialog(dialog);
  }
  if (!usingNativeModal) {
    document.removeEventListener('keydown', onEscapeKey);
  }
  restoreFocus();
});

function requestClose(): void {
  const dialog = dialogEl.value;
  if (!dialog || !isDialogOpen(dialog)) {
    return;
  }
  closeDialog(dialog);
  emit('close');
  restoreFocus();
}

function onCancel(event: Event): void {
  event.preventDefault();
  requestClose();
}
</script>

<template>
  <dialog
    ref="dialogEl"
    class="app-modal"
    aria-modal="true"
    :aria-labelledby="labelledBy"
    :aria-label="ariaLabel"
    @cancel="onCancel"
  >
    <button
      type="button"
      class="app-modal-dismiss"
      :aria-label="t('common.close')"
      @click="requestClose"
    />
    <div class="app-modal-panel" :class="{ 'app-modal-panel-wide': wide }">
      <slot />
    </div>
  </dialog>
</template>
