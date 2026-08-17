<script setup lang="ts">
// 表格单元格里的多名列表：先露出若干 chip，其余收进 +N，点开浮层只看被收起的那些。
import { computed } from 'vue';
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'reka-ui';
import { useI18n } from 'vue-i18n';
import Tooltip from '@/components/ui/Tooltip.vue';

const VISIBLE_COUNT = 2;

export interface OverflowChip {
  name: string;
  /** 出站主模型（chip 文本即实际上游名），虚线边框。 */
  canonical?: boolean;
  /** 清单名是别名时的出站主模型，虚线边框 +「实际请求」提示。 */
  actualRequest?: string;
  /** 额外提示（例如主模型上挂着的别名）。 */
  tooltip?: string;
}

const props = defineProps<{
  items: Array<string | OverflowChip>;
  chipTestId?: string;
}>();

const { t } = useI18n();

const chips = computed(() =>
  props.items.map((item) => (typeof item === 'string' ? { name: item } : item)),
);
const visible = computed(() => chips.value.slice(0, VISIBLE_COUNT));
const hidden = computed(() => chips.value.slice(VISIBLE_COUNT));
const hiddenCount = computed(() => hidden.value.length);

function isActualRequest(chip: OverflowChip): boolean {
  return Boolean(chip.canonical || chip.actualRequest);
}

function chipClass(chip: OverflowChip): string {
  return isActualRequest(chip) ? 'badge-canonical' : 'badge-info';
}

function chipTooltip(chip: OverflowChip): string {
  if (chip.canonical) return t('models.canonicalChipTooltip', { name: chip.name });
  if (chip.actualRequest) return t('models.canonicalChipTooltip', { name: chip.actualRequest });
  return chip.tooltip ?? '';
}
</script>

<template>
  <span v-if="chips.length === 0" class="text-fg-muted">{{ t('common.emptyCell') }}</span>
  <span v-else class="inline-flex max-w-full items-center gap-1">
    <Tooltip v-for="chip in visible" :key="chip.name" :text="chipTooltip(chip)">
      <span
        class="badge max-w-[9rem] truncate"
        :class="chipClass(chip)"
        :data-testid="chipTestId"
        :data-model="chip.name"
        :data-canonical="isActualRequest(chip) ? 'true' : undefined"
        :title="chipTooltip(chip) === '' ? chip.name : undefined"
      >
        {{ chip.name }}
      </span>
    </Tooltip>
    <PopoverRoot v-if="hiddenCount > 0">
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
          <ul class="overflow-chip-grid">
            <li v-for="chip in hidden" :key="chip.name" class="min-w-0">
              <Tooltip :text="chipTooltip(chip)">
                <span
                  class="badge max-w-full min-w-0 truncate"
                  :class="chipClass(chip)"
                  :data-testid="chipTestId"
                  :data-model="chip.name"
                  :data-canonical="isActualRequest(chip) ? 'true' : undefined"
                  :title="chipTooltip(chip) === '' ? chip.name : undefined"
                >
                  {{ chip.name }}
                </span>
              </Tooltip>
            </li>
          </ul>
        </PopoverContent>
      </PopoverPortal>
    </PopoverRoot>
  </span>
</template>
