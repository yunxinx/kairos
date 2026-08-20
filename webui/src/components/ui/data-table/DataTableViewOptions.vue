<script setup lang="ts" generic="Id extends string">
import {
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
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
  }>(),
  {
    testId: 'table-columns',
  },
);

const emit = defineEmits<{
  toggle: [id: Id, visible: boolean];
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
