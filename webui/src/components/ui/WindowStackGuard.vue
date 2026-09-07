<script setup lang="ts">
// 浮窗栈的脏关闭守卫确认窗：useWindowStack.close 触发守卫时置起
// pendingConfirmation，本组件按投影渲染 ConfirmWindow，确认后以 force
// 关闭被守卫窗口。各资源页在窗口渲染循环旁挂一份即可，无需自写守卫分支。
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import type { CloseGuardConfirmation } from '@/composables/useWindowStack';

const props = defineProps<{
  /** 栈的守卫确认投影；null 时不渲染。 */
  confirmation: CloseGuardConfirmation | null;
  /** 守卫确认窗的层叠序号（应高于栈内全部窗口）。 */
  stackOrder: number;
}>();

const emit = defineEmits<{
  /** 用户确认放弃：调用方以 force 关闭 confirmation.windowId。 */
  confirm: [windowId: number];
  /** 用户取消：调用方清除投影。 */
  cancel: [];
}>();

const { t } = useI18n();

// confirmKey 均为完整问句文案（表单草稿 / 一次性明文各有措辞），标题与
// 确认按钮按「丢的是什么」区分形态。
const isSecretConfirmation = computed(
  () => props.confirmation?.confirmKey === 'tokens.confirmCloseCreatedKey',
);
</script>

<template>
  <ConfirmWindow
    v-if="confirmation"
    :title="t(isSecretConfirmation ? 'tokens.createdKeyTitle' : 'common.unsavedChangesTitle')"
    :message="t(confirmation.confirmKey)"
    :stack-order="stackOrder"
    :topmost="true"
    confirm-test-id="close-guard-confirm"
    :confirm-label="
      t(isSecretConfirmation ? 'common.confirmDiscardAnyway' : 'common.confirmDiscard')
    "
    @close="emit('cancel')"
    @confirm="emit('confirm', confirmation.windowId)"
  />
</template>
