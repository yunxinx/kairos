<script setup lang="ts">
// 单选列表选择器：触发钮 + 带搜索的弹出列表。
// 选择、禁用、焦点和键盘导航由 Reka Combobox 原语统一管理；
// 本组件只负责项目的视觉结构和选项投影。
import { computed, ref, useAttrs, useId, watch } from 'vue';
import {
  ComboboxAnchor,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxInput,
  ComboboxItem,
  ComboboxItemIndicator,
  ComboboxPortal,
  ComboboxRoot,
  ComboboxTrigger,
  ComboboxViewport,
} from 'reka-ui';
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';
import type { ListboxSelectOption } from '@/lib/listbox-option';

defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    id: string;
    options: ListboxSelectOption[];
    invalid?: boolean;
    disabled?: boolean;
    /** 当前值无匹配选项时的降级显示文案。 */
    placeholder?: string;
    /** 搜索框占位与无障碍名。 */
    searchPlaceholder?: string;
    /** 叠加到浮层上的类名（如加宽以容纳冗长说明行）。 */
    menuClass?: string;
    /** 选项上的 data-testid 前缀；不传则只给触发器上的外层 testid。 */
    testId?: string;
  }>(),
  {
    invalid: false,
    disabled: false,
    placeholder: '',
    searchPlaceholder: '',
    menuClass: '',
    testId: '',
  },
);

const modelValue = defineModel<string>({ required: true });
const { t } = useI18n();
const attrs = useAttrs();
const uid = useId();

const open = ref(false);
const query = ref('');
const searchId = `${props.id}-search-${uid}`;
const emptySearchDisplay = () => '';
const triggerAriaLabel = computed(() => {
  const label = attrs['aria-label'];
  return typeof label === 'string' ? label : props.searchPlaceholder || undefined;
});

const selectedLabel = computed(() => {
  const option = props.options.find((item) => item.value === modelValue.value);
  if (option) return option.label;
  // 当前值不在名单里（如已撤组仍绑定的令牌）：降级显示原值，与 UiSelect 行为一致。
  return modelValue.value;
});

function optionSearchText(option: ListboxSelectOption): string {
  return [option.label, option.value, option.description].filter(Boolean).join(' ');
}

function onOpenChange(value: boolean) {
  if (props.disabled && value) return;
  open.value = value;
}

watch(
  [() => props.disabled, open],
  ([disabled, isOpen]) => {
    if (disabled && isOpen) open.value = false;
    if (!isOpen) query.value = '';
  },
  { flush: 'sync' },
);
</script>

<template>
  <div class="listbox-select">
    <ComboboxRoot
      v-model="modelValue"
      :open="open"
      :disabled="disabled"
      :reset-search-term-on-blur="true"
      :reset-search-term-on-select="true"
      @update:open="onOpenChange"
    >
      <ComboboxAnchor as-child>
        <ComboboxTrigger
          v-bind="attrs"
          :id="id"
          :aria-label="triggerAriaLabel"
          tabindex="0"
          class="ui-select-trigger"
          :class="{ 'input-invalid': invalid }"
          :data-placeholder="selectedLabel ? undefined : ''"
        >
          <span class="ui-select-copy">
            <span class="ui-select-value" :class="{ 'text-fg-muted': !selectedLabel }">
              {{ selectedLabel || placeholder }}
            </span>
          </span>
          <UiIcon name="chevron-down" class="ui-select-icon" :size="14" />
        </ComboboxTrigger>
      </ComboboxAnchor>
      <ComboboxPortal>
        <ComboboxContent
          position="popper"
          align="start"
          :side-offset="4"
          :class="['listbox-select-menu', menuClass]"
        >
          <div class="faceted-filter-search">
            <UiIcon name="search" :size="14" class="text-fg-subtle shrink-0" />
            <ComboboxInput
              :id="searchId"
              v-model="query"
              :display-value="emptySearchDisplay"
              class="faceted-filter-search-field"
              :placeholder="searchPlaceholder"
              :aria-label="searchPlaceholder"
              :data-testid="testId ? `${testId}-search` : undefined"
            />
          </div>
          <ComboboxViewport class="listbox-select-list" :aria-label="searchPlaceholder">
            <ComboboxEmpty class="text-fg-muted px-2 py-4 text-center text-sm">
              {{ t('common.filterEmpty') }}
            </ComboboxEmpty>
            <ComboboxItem
              v-for="option in options"
              :key="option.value"
              :value="option.value"
              :text-value="optionSearchText(option)"
              class="listbox-option"
              :data-testid="testId ? `${testId}-option` : undefined"
              :data-value="option.value"
            >
              <span
                class="sync-filter-box"
                :data-active="String(option.value === modelValue)"
                aria-hidden="true"
              >
                <ComboboxItemIndicator>
                  <UiIcon name="check" :size="10" />
                </ComboboxItemIndicator>
              </span>
              <span class="min-w-0 flex-1">
                <span class="listbox-option-name">{{ option.label }}</span>
                <span v-if="option.description" class="listbox-option-desc">{{
                  option.description
                }}</span>
              </span>
              <span v-if="option.badge" class="badge badge-neutral listbox-option-badge">
                {{ option.badge }}
              </span>
            </ComboboxItem>
          </ComboboxViewport>
        </ComboboxContent>
      </ComboboxPortal>
    </ComboboxRoot>
  </div>
</template>
