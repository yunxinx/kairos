<script setup lang="ts">
// 组员列：双栏最多 3 行（6 格），再多时 +N 占最后一格。
// 统一模型在列表里只出身份名；成员构成去编辑器看。
import { computed } from 'vue';
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'reka-ui';
import { useI18n } from 'vue-i18n';
import { overflowGridItems } from '@/lib/overflow-grid';
import type { CallableSourceLine } from '@/lib/unified-sources';
import ModelSourceLine from '@/features/models/ModelSourceLine.vue';

const props = withDefaults(
  defineProps<{
    lines: CallableSourceLine[];
    chipTestId?: string;
  }>(),
  { chipTestId: 'group-source-channel' },
);

const { t } = useI18n();

const sliced = computed(() => overflowGridItems(props.lines));
const hiddenCount = computed(() => sliced.value.hidden.length);
</script>

<template>
  <ul
    v-if="lines.length > 0"
    class="m-0 grid max-w-full list-none grid-cols-2 gap-1.5 p-0"
    data-testid="model-source-lines"
  >
    <ModelSourceLine
      v-for="line in sliced.visible"
      :key="line.key"
      :line="line"
      :chip-test-id="chipTestId"
    />
    <li v-if="hiddenCount > 0" class="flex items-center">
      <PopoverRoot>
        <PopoverTrigger as-child>
          <button
            type="button"
            class="badge badge-neutral cursor-pointer"
            data-testid="overflow-more"
            :aria-label="t('common.moreCount', { count: hiddenCount })"
          >
            {{ t('common.moreCount', { count: hiddenCount }) }}
          </button>
        </PopoverTrigger>
        <PopoverPortal>
          <PopoverContent
            align="start"
            :side-offset="4"
            class="data-table-menu overflow-chip-menu seed-scrollbar"
            data-testid="overflow-chip-menu"
          >
            <ul class="m-0 grid list-none grid-cols-2 gap-1.5 p-1">
              <ModelSourceLine
                v-for="line in sliced.hidden"
                :key="line.key"
                :line="line"
                :chip-test-id="chipTestId"
              />
            </ul>
          </PopoverContent>
        </PopoverPortal>
      </PopoverRoot>
    </li>
  </ul>
  <span v-else class="text-fg-muted">{{ t('common.emptyCell') }}</span>
</template>
