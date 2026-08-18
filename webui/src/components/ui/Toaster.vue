<script setup lang="ts">
// 右上角 Toast 视口：Reka Toast 负责倒计时、悬停暂停、滑动关闭与读屏宣告。
import { ToastClose, ToastDescription, ToastProvider, ToastRoot, ToastViewport } from 'reka-ui';
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';
import { cn } from '@/lib/cn';
import { TOAST_DURATION_MS, removeClosedToast, useToast } from '@/composables/useToast';

const { t } = useI18n();
const { toasts } = useToast();
</script>

<template>
  <ToastProvider
    :duration="TOAST_DURATION_MS"
    :label="t('a11y.toastLabel')"
    swipe-direction="right"
  >
    <ToastRoot
      v-for="item in toasts"
      :key="item.id"
      v-model:open="item.open"
      class="toast-item"
      :class="cn(item.kind === 'success' ? 'toast-item-success' : 'toast-item-danger')"
      :duration="item.duration"
      :type="item.kind === 'danger' ? 'foreground' : 'background'"
      data-testid="toast"
      :data-kind="item.kind"
      @update:open="(open: boolean) => open || removeClosedToast(item.id)"
    >
      <UiIcon
        :name="item.kind === 'success' ? 'check' : 'circle-alert'"
        class="toast-item-icon"
        :size="16"
      />
      <ToastDescription class="toast-item-message">{{ item.message }}</ToastDescription>
      <ToastClose class="toast-item-close" :aria-label="t('common.close')">
        <UiIcon name="close" :size="14" />
      </ToastClose>
    </ToastRoot>
    <ToastViewport
      class="toast-viewport"
      :label="(hotkey) => t('a11y.toastViewport', { hotkey })"
    />
  </ToastProvider>
</template>
