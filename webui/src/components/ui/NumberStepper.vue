<script setup lang="ts">
// 表格内联数字步进：平时只露出数值，悬停/聚焦时两侧 +/- 按钮淡入。
// 提交时机：按钮即时提交；输入框在失焦或回车时提交，非法值回滚为当前值。
import { ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';

const props = withDefaults(
  defineProps<{
    /** 允许的最小值（含）；触底时减少按钮禁用。 */
    min: number;
    disabled?: boolean;
    /** 输入框的无障碍标签。 */
    label: string;
  }>(),
  { disabled: false },
);

const model = defineModel<number>({ required: true });

const { t } = useI18n();

const draft = ref(String(model.value));
watch(model, (value) => {
  draft.value = String(value);
});

function commit() {
  const parsed = Number(draft.value);
  if (!Number.isInteger(parsed) || parsed < props.min) {
    draft.value = String(model.value);
    return;
  }
  if (parsed !== model.value) {
    model.value = parsed;
  } else {
    draft.value = String(model.value);
  }
}

function step(delta: number) {
  const next = model.value + delta;
  if (next < props.min) return;
  model.value = next;
}
</script>

<template>
  <span class="number-stepper group inline-flex items-center gap-0.5">
    <button
      type="button"
      class="icon-btn text-fg-muted number-stepper-btn p-1"
      :disabled="disabled || model <= min"
      :aria-label="t('common.decrement')"
      :title="t('common.decrement')"
      @click="step(-1)"
    >
      <UiIcon name="minus" :size="12" />
    </button>
    <input
      v-model="draft"
      type="text"
      inputmode="numeric"
      class="number-stepper-input w-14 rounded border border-transparent bg-transparent px-1 py-0.5 text-center font-mono text-sm transition-colors group-hover:border-[var(--seed-border)] focus:border-[var(--seed-primary)] focus:outline-none disabled:opacity-60"
      :disabled="disabled"
      :aria-label="label"
      @change="commit"
      @keydown.enter="($event.target as HTMLInputElement).blur()"
    />
    <button
      type="button"
      class="icon-btn text-fg-muted number-stepper-btn p-1"
      :disabled="disabled"
      :aria-label="t('common.increment')"
      :title="t('common.increment')"
      @click="step(1)"
    >
      <UiIcon name="plus" :size="12" />
    </button>
  </span>
</template>

<style scoped>
/* 按钮占位常驻、透明度切换：避免悬停时布局跳动，且保持键盘可聚焦。 */
.number-stepper-btn {
  display: inline-flex;
  align-items: center;
  opacity: 0;
  transition: opacity 150ms ease-in-out;
}
.number-stepper-btn:disabled {
  cursor: not-allowed;
  opacity: 0;
}
.number-stepper:hover .number-stepper-btn:not(:disabled),
.number-stepper:focus-within .number-stepper-btn:not(:disabled) {
  opacity: 1;
}
.number-stepper-btn:hover:not(:disabled) {
  background: var(--seed-surface-alt);
  color: var(--seed-fg);
}
</style>
