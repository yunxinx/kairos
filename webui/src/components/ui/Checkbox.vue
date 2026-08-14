<script setup lang="ts">
// 复选框：Reka UI CheckboxRoot 套 ui-checkbox 样式；支持半选（indeterminate）三态。
import { computed } from 'vue';
import { CheckboxIndicator, CheckboxRoot } from 'reka-ui';
import UiIcon from '@/components/ui/UiIcon.vue';

defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    /** 半选态：表头全选框在部分行选中时的第三态。 */
    indeterminate?: boolean;
  }>(),
  {
    indeterminate: false,
  },
);

const model = defineModel<boolean>({ required: true });

// reka-ui 以 'indeterminate' 字面量表达半选；点击半选框视为选中。
const checked = computed<boolean | 'indeterminate'>({
  get: () => (props.indeterminate ? 'indeterminate' : model.value),
  set: (value) => {
    model.value = value === true || value === 'indeterminate';
  },
});
</script>

<template>
  <CheckboxRoot v-bind="$attrs" v-model="checked" class="ui-checkbox">
    <span class="ui-checkbox__control">
      <CheckboxIndicator class="ui-checkbox__indicator">
        <UiIcon class="ui-checkbox__icon" :name="props.indeterminate ? 'minus' : 'check'" />
      </CheckboxIndicator>
    </span>
  </CheckboxRoot>
</template>
