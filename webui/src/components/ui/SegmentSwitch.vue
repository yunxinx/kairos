<script setup lang="ts" generic="T extends string">
// 双选项分段胶囊开关：视觉见 globals.css 的 .segment-switch，滑块随选中项左右位移。
import { computed } from 'vue';

export interface SegmentOption<T extends string> {
  value: T;
  label: string;
  /** Playwright 选择器契约，原样透传到按钮的 data-testid。 */
  testId?: string;
}

/** 样式按两项等分布局，滑块只左右位移，故选项固定为一对。 */
export type SegmentPair<T extends string> = [SegmentOption<T>, SegmentOption<T>];

const props = defineProps<{
  modelValue: T;
  options: SegmentPair<T>;
  /** 提交中的表单锁定选择，避免改变已发出的命令。 */
  disabled?: boolean;
  /** 开关组的可访问名称；vue-tsc 不把模板 :aria-label 映射为驼峰 prop，故按原名声明。 */
  // eslint-disable-next-line vue/prop-name-casing
  'aria-label': string;
}>();

const emit = defineEmits<{
  'update:modelValue': [value: T];
}>();

const knobRight = computed(() => props.modelValue === props.options[1].value);
</script>

<template>
  <div
    class="segment-switch shrink-0"
    :data-knob="knobRight ? 'right' : 'left'"
    role="group"
    :aria-label="props['aria-label']"
    :aria-disabled="props.disabled ? 'true' : undefined"
  >
    <button
      v-for="option in options"
      :key="option.value"
      type="button"
      class="segment-switch-btn"
      :data-testid="option.testId"
      :aria-pressed="modelValue === option.value"
      :disabled="props.disabled"
      @click="emit('update:modelValue', option.value)"
    >
      {{ option.label }}
    </button>
  </div>
</template>
