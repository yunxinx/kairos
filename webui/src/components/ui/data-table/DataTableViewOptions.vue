<script setup lang="ts" generic="Id extends string">
import {
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuItemIndicator,
  DropdownMenuLabel,
  DropdownMenuPortal,
  DropdownMenuRoot,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from 'reka-ui';
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';
import type { ColumnVisibilityItem } from '@/composables/useColumnVisibility';

withDefaults(
  defineProps<{
    items: ColumnVisibilityItem<Id>[];
    labels: Record<Id, string>;
    testId?: string;
    /** 页面接了列宽拖拽时才渲染「重置列宽」入口。 */
    resetWidths?: boolean;
  }>(),
  {
    testId: 'table-columns',
    resetWidths: false,
  },
);

const emit = defineEmits<{
  toggle: [id: Id, visible: boolean];
  resetWidths: [];
}>();

const { t } = useI18n();
</script>

<template>
  <DropdownMenuRoot :modal="false">
    <DropdownMenuTrigger as-child>
      <button
        type="button"
        class="btn btn-subtle"
        :data-testid="testId"
        :aria-label="t('common.toggleColumns')"
      >
        <UiIcon name="sliders-horizontal" :size="14" />
        {{ t('common.columns') }}
      </button>
    </DropdownMenuTrigger>
    <DropdownMenuPortal>
      <DropdownMenuContent class="data-table-menu" align="end" :side-offset="4">
        <DropdownMenuLabel class="data-table-menu-label">
          {{ t('common.toggleColumns') }}
        </DropdownMenuLabel>
        <DropdownMenuSeparator class="data-table-menu-separator" />
        <DropdownMenuItem
          v-if="resetWidths"
          class="data-table-menu-item"
          :data-testid="`${testId}-reset-widths`"
          @select="emit('resetWidths')"
        >
          <UiIcon name="refresh-cw" :size="14" />
          {{ t('common.resetColumnWidths') }}
        </DropdownMenuItem>
        <DropdownMenuCheckboxItem
          v-for="item in items"
          :key="item.id"
          class="data-table-menu-item data-table-menu-checkbox"
          :model-value="item.checked"
          :disabled="item.disabled"
          :data-testid="`${testId}-option`"
          :data-value="item.id"
          @select.prevent
          @update:model-value="(value) => emit('toggle', item.id, value === true)"
        >
          <span class="data-table-menu-checkbox-indicator">
            <DropdownMenuItemIndicator>
              <UiIcon name="check" :size="14" />
            </DropdownMenuItemIndicator>
          </span>
          {{ labels[item.id] }}
        </DropdownMenuCheckboxItem>
      </DropdownMenuContent>
    </DropdownMenuPortal>
  </DropdownMenuRoot>
</template>
