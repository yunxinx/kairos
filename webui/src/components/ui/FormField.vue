<script setup lang="ts">
import { computed, useSlots } from 'vue';
import FieldInfoHint from '@/components/ui/FieldInfoHint.vue';

const props = withDefaults(
  defineProps<{
    label: string;
    inputId: string;
    fieldName: string;
    error?: string | undefined;
    /** 为 false 时标题仅作展示，控件通过 slot 内 aria-labelledby 关联（如自定义 Select）。 */
    labelsControl?: boolean;
    /** 标题右侧格式说明；与 #guide 插槽二选一，插槽优先。 */
    guide?: string | undefined;
  }>(),
  {
    labelsControl: true,
    error: undefined,
    guide: undefined,
  },
);

const slots = useSlots();

const hintId = computed(() => `${props.inputId}-error`);
const labelId = computed(() => `${props.inputId}-label`);
const guideContentId = computed(() => `${props.inputId}-guide`);
const hasGuide = computed(() => Boolean(props.guide) || Boolean(slots.guide));
</script>

<template>
  <div class="form-field" :data-form-field="fieldName">
    <div class="form-field-label-row">
      <label v-if="labelsControl" :for="inputId" class="form-field-label">{{ label }}</label>
      <span v-else :id="labelId" class="form-field-label">{{ label }}</span>
      <FieldInfoHint v-if="hasGuide" :content-id="guideContentId">
        <slot name="guide">
          <p v-if="guide" class="field-info-hint-text">{{ guide }}</p>
        </slot>
      </FieldInfoHint>
    </div>
    <div class="form-field-control">
      <slot
        :label-id="labelId"
        :hint-id="hintId"
        :has-error="Boolean(error)"
        :invalid="Boolean(error)"
      />
      <Transition name="form-hint">
        <p v-if="error" :id="hintId" class="form-field-hint" role="alert">
          {{ error }}
        </p>
      </Transition>
    </div>
  </div>
</template>
