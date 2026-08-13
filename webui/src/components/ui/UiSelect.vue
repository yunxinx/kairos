<script setup lang="ts">
import { onScopeDispose, ref, useAttrs, watch } from 'vue';
import {
  SelectContent,
  SelectItem,
  SelectItemIndicator,
  SelectItemText,
  SelectPortal,
  SelectRoot,
  SelectScrollDownButton,
  SelectScrollUpButton,
  SelectTrigger,
  SelectValue,
  SelectViewport,
} from 'reka-ui';
import UiIcon from '@/components/ui/UiIcon.vue';

defineOptions({
  inheritAttrs: false,
});

/** 下拉选项，value 与 label 分离以便 i18n 展示。 */
export interface UiSelectOption {
  value: string;
  label: string;
}

withDefaults(
  defineProps<{
    id: string;
    options: UiSelectOption[];
    invalid?: boolean;
    disabled?: boolean;
  }>(),
  {
    invalid: false,
    disabled: false,
  },
);

const attrs = useAttrs();

const modelValue = defineModel<string>({ required: true });

const emit = defineEmits<{
  pointerdown: [event: PointerEvent];
}>();

const open = ref(false);

function releaseFocus() {
  (document.activeElement as HTMLElement | null)?.blur();
}

function handlePointerdown(event: PointerEvent) {
  emit('pointerdown', event);
}

function handleCloseAutoFocus(event: Event) {
  event.preventDefault();
  releaseFocus();
}

watch(open, (isOpen) => {
  if (!isOpen) {
    releaseFocus();
  }
});

onScopeDispose(() => {
  open.value = false;
  releaseFocus();
});
</script>

<template>
  <div class="ui-select">
    <SelectRoot v-model="modelValue" v-model:open="open" :disabled="disabled">
      <SelectTrigger
        v-bind="attrs"
        :id="id"
        class="ui-select-trigger"
        :class="{ 'input-invalid': invalid }"
        @pointerdown="handlePointerdown"
      >
        <span class="ui-select-copy">
          <SelectValue class="ui-select-value" />
        </span>
        <UiIcon name="chevron-down" class="ui-select-icon" :size="14" />
      </SelectTrigger>

      <SelectPortal>
        <SelectContent
          class="ui-select-content"
          position="item-aligned"
          :body-lock="false"
          @close-auto-focus="handleCloseAutoFocus"
        >
          <SelectScrollUpButton class="ui-select-scroll-btn" aria-label="Scroll up">
            <UiIcon name="chevron-up" :size="14" />
          </SelectScrollUpButton>

          <SelectViewport class="ui-select-viewport">
            <SelectItem
              v-for="option in options"
              :key="option.value"
              :value="option.value"
              class="ui-select-item"
            >
              <span class="ui-select-item-text">
                <SelectItemText>{{ option.label }}</SelectItemText>
              </span>
              <span class="ui-select-item-indicator">
                <SelectItemIndicator>
                  <UiIcon name="check" :size="16" />
                </SelectItemIndicator>
              </span>
            </SelectItem>
          </SelectViewport>

          <SelectScrollDownButton class="ui-select-scroll-btn" aria-label="Scroll down">
            <UiIcon name="chevron-down" :size="14" />
          </SelectScrollDownButton>
        </SelectContent>
      </SelectPortal>
    </SelectRoot>
  </div>
</template>
