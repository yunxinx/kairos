<script setup lang="ts">
// 用户自己的模型清单：按套餐模型组分段的只读表，单价已是折后价。
//
// 数据源是 `/me/models`，与下游 `GET /v1/models` 同一套名单规则；刻意不复用
// `/model-groups` + `/channels/summary` 那条 admin 路径——组的原始形状带渠道拓扑，
// 普通用户不该看到，而且那两个端点对他本来就是 403。
import { computed, ref } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { MyModelView, PriceRange } from '@/api/types';
import CopyableName from '@/components/ui/CopyableName.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import InlineError from '@/components/ui/InlineError.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import UnifiedNameChip from '@/features/models/UnifiedNameChip.vue';
import { formatDiscountBp, formatUsdMicros } from '@/lib/format';
import { DEFAULT_MODEL_GROUP } from '@/lib/visible-models';

const { t } = useI18n();
const searchText = ref('');

const myModelsQuery = useQuery({
  queryKey: ['my-models'],
  queryFn: () => apiClient.listMyModels(),
});

const discountBp = computed(() => myModelsQuery.data.value?.discount_bp ?? 10_000);
/** 非原价时才在工具栏标注折扣，避免给「100%」这种无信息量的徽章。 */
const showDiscount = computed(() => discountBp.value !== 10_000);

const sections = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  return (myModelsQuery.data.value?.groups ?? []).map((group) => ({
    name: group.name,
    label: group.name === DEFAULT_MODEL_GROUP ? t('models.ungrouped') : group.name,
    models: group.models.filter((model) => !q || model.id.toLowerCase().includes(q)),
  }));
});

/** 搜索后仍有内容的段；全空时整表给空态。 */
const visibleSections = computed(() => sections.value.filter((s) => s.models.length > 0));

/**
 * 空态分两种：搜索把结果滤空了，还是这个套餐本来就没有可调用模型。
 *
 * 混成一句会在用户随手搜个词之后告诉他「你的套餐没有模型组」——那是假警报，
 * 会让人以为权限被撤了。
 */
const searchFilteredAll = computed(() => {
  if (searchText.value.trim() === '') return false;
  // 判据是「过滤前本来有模型」，不是「本来有组」：套餐挂了空组时，即便正在搜索，
  // 诚实的说法仍是「你的套餐没有可调用模型」。
  return (myModelsQuery.data.value?.groups ?? []).some((group) => group.models.length > 0);
});

const totalCount = computed(() => {
  const ids = new Set<string>();
  for (const section of visibleSections.value) {
    for (const model of section.models) ids.add(model.id);
  }
  return ids.size;
});

const showTableSkeleton = computed(
  () => myModelsQuery.isPending.value && !myModelsQuery.data.value,
);

/** 区间两端相等时只显示一个数：单渠道的名字不该看起来像有浮动。 */
function priceText(range: PriceRange | undefined): string | null {
  if (!range) return null;
  if (range.min_micros === range.max_micros) return formatUsdMicros(range.min_micros);
  return `${formatUsdMicros(range.min_micros)}–${formatUsdMicros(range.max_micros)}`;
}

function tierText(
  model: MyModelView,
  tier: 'input' | 'output' | 'cache_read' | 'cache_write' | 'cache_write_1h',
) {
  return priceText(model[tier]);
}
</script>

<template>
  <div class="flex flex-col">
    <InlineError
      v-if="myModelsQuery.isError.value && !myModelsQuery.data.value"
      :message="extractApiError(myModelsQuery.error.value).message"
      @retry="() => myModelsQuery.refetch()"
    />
    <div v-else class="flex flex-col">
      <!-- 页面的要点是「这些名字就是请求里的 model」；不写出来，用户拿到一张表也不知道怎么用。 -->
      <p class="text-fg-muted mb-4 max-w-2xl text-sm" data-testid="my-models-lead">
        {{ t('models.myLead') }}
      </p>
      <DataTable data-testid="my-models-table" :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <SearchInput
              id="my-models-search"
              v-model="searchText"
              class="max-w-sm"
              data-testid="my-models-search"
              :placeholder="t('models.searchVisible')"
              :aria-label="t('models.searchVisible')"
            />
            <template #actions>
              <span
                v-if="showDiscount"
                class="badge badge-info font-mono"
                data-testid="my-models-discount"
                :title="t('models.myDiscountHint', { rate: formatDiscountBp(discountBp) })"
              >
                {{ t('models.myDiscount', { rate: formatDiscountBp(discountBp) }) }}
              </span>
            </template>
          </DataTableToolbar>
        </template>
        <TableHeader>
          <TableRow>
            <TableHead>
              <span class="inline-flex items-center gap-1.5">
                {{ t('pricing.model') }}
                <span
                  class="badge badge-neutral !bg-[var(--seed-surface)] font-mono"
                  data-testid="my-models-count"
                  :aria-label="t('models.modelCount', { count: totalCount })"
                >
                  {{ totalCount }}
                </span>
              </span>
            </TableHead>
            <TableHead align="right">{{ t('models.myPriceInput') }}</TableHead>
            <TableHead align="right">{{ t('models.myPriceOutput') }}</TableHead>
            <TableHead align="right">{{ t('models.myPriceCacheRead') }}</TableHead>
            <TableHead align="right">{{ t('models.myPriceCacheWrite') }}</TableHead>
            <TableHead align="right">{{ t('models.myPriceCacheWrite1h') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="6" />
          <template v-else>
            <template v-for="section in visibleSections" :key="section.name">
              <TableRow
                class="inventory-section-row"
                data-testid="my-models-section"
                :data-group="section.name"
              >
                <TableCell :colspan="6" class="inventory-section-cell">
                  {{ section.label }}
                </TableCell>
              </TableRow>
              <TableRow
                v-for="model in section.models"
                :key="`${section.name}:${model.id}`"
                data-testid="my-model"
                :data-model="model.id"
                :data-group="section.name"
              >
                <TableCell class="min-w-0 font-mono font-medium">
                  <span class="inline-flex min-w-0 items-center gap-1.5">
                    <UnifiedNameChip v-if="model.unified" :name="model.id" />
                    <CopyableName v-else :text="model.id" test-id="my-model-name" />
                    <span
                      v-if="!model.callable"
                      class="badge badge-warn shrink-0 whitespace-nowrap"
                      data-testid="my-model-uncallable"
                      :title="t('models.myUncallableHint')"
                    >
                      {{ t('models.myUncallable') }}
                    </span>
                  </span>
                </TableCell>
                <TableCell align="right" class="font-mono" data-testid="my-model-input">
                  <span v-if="tierText(model, 'input')">{{ tierText(model, 'input') }}</span>
                  <span v-else class="text-fg-muted">{{ t('common.emptyCell') }}</span>
                </TableCell>
                <TableCell align="right" class="font-mono" data-testid="my-model-output">
                  <span v-if="tierText(model, 'output')">{{ tierText(model, 'output') }}</span>
                  <span v-else class="text-fg-muted">{{ t('common.emptyCell') }}</span>
                </TableCell>
                <TableCell align="right" class="font-mono">
                  <span v-if="tierText(model, 'cache_read')">
                    {{ tierText(model, 'cache_read') }}
                  </span>
                  <span v-else class="text-fg-muted">{{ t('common.emptyCell') }}</span>
                </TableCell>
                <TableCell align="right" class="font-mono">
                  <span v-if="tierText(model, 'cache_write')">
                    {{ tierText(model, 'cache_write') }}
                  </span>
                  <span v-else class="text-fg-muted">{{ t('common.emptyCell') }}</span>
                </TableCell>
                <TableCell align="right" class="font-mono">
                  <span v-if="tierText(model, 'cache_write_1h')">
                    {{ tierText(model, 'cache_write_1h') }}
                  </span>
                  <span v-else class="text-fg-muted">{{ t('common.emptyCell') }}</span>
                </TableCell>
              </TableRow>
            </template>
            <!-- 两种空态分开：搜不到 ≠ 套餐里没有。后者才该提示去找管理员。 -->
            <TableRow v-if="visibleSections.length === 0">
              <TableCell :colspan="6" class="h-24 whitespace-normal">
                <EmptyState
                  v-if="searchFilteredAll"
                  :title="t('models.mySearchEmpty')"
                  data-testid="my-models-search-empty"
                />
                <EmptyState
                  v-else
                  :title="t('models.myEmpty')"
                  :description="t('models.myEmptyHint')"
                  data-testid="my-models-empty"
                />
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
      <p class="text-fg-muted mt-3 text-xs" data-testid="my-models-footnote">
        {{ t('models.myPriceUnit') }}
      </p>
    </div>
  </div>
</template>
