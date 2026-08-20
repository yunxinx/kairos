<script setup lang="ts">
import {
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuPortal,
  DropdownMenuRoot,
  DropdownMenuTrigger,
} from 'reka-ui';
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type { SortDir } from '@/api/types';
import UiIcon from '@/components/ui/UiIcon.vue';
import type { IconName } from '@/components/ui/icon-paths';

const props = withDefaults(
  defineProps<{
    label: string;
    sorted?: SortDir | false;
    /** 当前列是用户选的排序（非缺省）时，再点表头取消，不打开菜单。 */
    clearable?: boolean;
  }>(),
  {
    sorted: false,
    clearable: false,
  },
);

const emit = defineEmits<{
  sort: [dir: SortDir];
  clear: [];
}>();

const { t } = useI18n();

const canClear = computed(() => props.sorted !== false && props.clearable);

const sortIcon = computed((): IconName => {
  if (props.sorted === 'asc') return 'arrow-up';
  if (props.sorted === 'desc') return 'arrow-down';
  return 'chevrons-up-down';
});
</script>

<template>
  <button
    v-if="canClear"
    type="button"
    class="data-table-column-header"
    :data-sorted="sorted"
    data-clearable
    data-testid="column-clear-sort"
    :title="t('common.clearSort')"
    @click="emit('clear')"
  >
    <span>{{ label }}</span>
    <span class="data-table-column-header-affordance" aria-hidden="true">
      <UiIcon :name="sortIcon" class="data-table-column-header-caret" :size="12" />
      <UiIcon name="close" class="data-table-column-header-clear" :size="12" />
    </span>
  </button>
  <DropdownMenuRoot v-else :modal="false">
    <DropdownMenuTrigger as-child>
      <button
        type="button"
        class="data-table-column-header"
        :data-sorted="sorted === false ? undefined : sorted"
      >
        <span>{{ label }}</span>
        <UiIcon :name="sortIcon" class="data-table-column-header-caret" :size="12" />
      </button>
    </DropdownMenuTrigger>
    <DropdownMenuPortal>
      <DropdownMenuContent class="data-table-menu" align="start" :side-offset="4">
        <DropdownMenuItem
          class="data-table-menu-item"
          data-testid="column-sort-asc"
          @select="emit('sort', 'asc')"
        >
          <UiIcon name="arrow-up" :size="14" class="text-fg-muted" />
          {{ t('common.sortAscending') }}
        </DropdownMenuItem>
        <DropdownMenuItem
          class="data-table-menu-item"
          data-testid="column-sort-desc"
          @select="emit('sort', 'desc')"
        >
          <UiIcon name="arrow-down" :size="14" class="text-fg-muted" />
          {{ t('common.sortDescending') }}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenuPortal>
  </DropdownMenuRoot>
</template>
