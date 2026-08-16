<script setup lang="ts">
// 危险操作确认浮窗：与编辑窗口共用窗口栈，确认后由调用方执行并在成功时关闭。
import { computed, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    title: string;
    message: string;
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
    cascade?: number;
    attention?: boolean;
    topmost?: boolean;
    /** 执行失败时展示的错误文案。 */
    error?: string;
    /** 确认请求进行中时禁用按钮。 */
    busy?: boolean;
    /** 确认按钮 data-testid，保持各资源页既有选择器契约。 */
    confirmTestId?: string | undefined;
    /** 确认按钮文案；缺省为删除确认。 */
    confirmLabel?: string | undefined;
  }>(),
  {
    anchor: null,
    stackOrder: 0,
    cascade: 0,
    attention: false,
    topmost: true,
    error: '',
    busy: false,
    confirmTestId: undefined,
    confirmLabel: undefined,
  },
);

const emit = defineEmits<{
  close: [];
  confirm: [];
  raise: [];
  'dirty-change': [dirty: boolean];
}>();

const { t } = useI18n();

// 请求进行中或展示错误时不可被淘汰，否则操作状态与错误信息会凭空丢失。
const dirty = computed(() => props.busy || props.error !== '');
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });
</script>

<template>
  <FloatingWindow
    :title="title"
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <div class="card-body space-y-3">
      <p class="text-sm">{{ message }}</p>
      <p v-if="error" class="text-danger text-sm">{{ error }}</p>
    </div>
    <div class="card-footer card-body flex justify-between gap-2">
      <button type="button" class="btn" @click="emit('close')">
        {{ t('common.cancel') }}
      </button>
      <button
        type="button"
        class="btn btn-danger-filled"
        :data-testid="confirmTestId"
        :disabled="busy"
        @click="emit('confirm')"
      >
        {{ confirmLabel ?? t('common.confirmDelete') }}
      </button>
    </div>
  </FloatingWindow>
</template>
