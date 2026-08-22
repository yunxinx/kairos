<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { roleAtLeast, type UserAdminView } from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import InlineError from '@/components/ui/InlineError.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import { useToast } from '@/composables/useToast';
import { groupDisplayName } from '@/lib/visible-models';

const props = defineProps<{
  user: UserAdminView;
}>();

const emit = defineEmits<{
  close: [];
  'dirty-change': [dirty: boolean];
}>();

const { t } = useI18n();
const { error, success } = useToast();
const queryClient = useQueryClient();

// admin+ 的令牌准入不经过可用组名单（token_group_assigned 直接放行），分配对其
// 无效；只有普通用户才编辑名单。
const unrestricted = computed(() => roleAtLeast(props.user.role, 'admin'));

const selected = ref<string[]>([...props.user.assigned_groups]);
const filterText = ref('');

const groupsQuery = useQuery({
  queryKey: ['model-groups'],
  queryFn: () => apiClient.listModelGroups(),
  enabled: !unrestricted.value,
});

const allGroupNames = computed(() => {
  const names = (groupsQuery.data.value ?? []).map((group) => group.name);
  for (const extra of selected.value) {
    if (!names.includes(extra)) names.push(extra);
  }
  return names.sort((left, right) => left.localeCompare(right));
});

const filteredGroupNames = computed(() => {
  const q = filterText.value.trim().toLowerCase();
  if (!q) return allGroupNames.value;
  return allGroupNames.value.filter((name) => name.toLowerCase().includes(q));
});

/** 本次保存将移除的组；非空时须先过影响确认（撤组令牌立即失效，ADR-0010）。 */
const removedGroups = computed(() =>
  props.user.assigned_groups.filter((name) => !selected.value.includes(name)),
);

/** 影响确认态：null 为普通态；数字为将被波及的令牌数；'unknown' 为计数拉取失败。 */
const revokeImpact = ref<number | 'unknown' | null>(null);
/** 影响数拉取进行中：禁用保存按钮，防连点并发请求。 */
const impactLoading = ref(false);
/** 选择版本号：拉取期间选择又变时，丢弃按旧选择算出的过期计数。 */
let selectionVersion = 0;
watch(selected, () => {
  selectionVersion += 1;
  revokeImpact.value = null;
});

const dirty = computed(() => {
  if (unrestricted.value) return false;
  const next = [...selected.value].sort();
  const prev = [...props.user.assigned_groups].sort();
  return next.length !== prev.length || next.some((name, index) => name !== prev[index]);
});
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

function isSelected(name: string): boolean {
  return selected.value.includes(name);
}

function toggleGroup(name: string, checked: boolean) {
  if (checked) {
    if (!selected.value.includes(name)) selected.value = [...selected.value, name];
    return;
  }
  selected.value = selected.value.filter((item) => item !== name);
}

function selectAll() {
  selected.value = [...allGroupNames.value];
}

function clearAll() {
  selected.value = [];
}

const saveMutation = useMutation({
  mutationFn: () => apiClient.replaceUserModelGroups(props.user.id, selected.value),
  onSuccess: async () => {
    success(t('account.profileSaved'));
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['users'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

/** 保存：有移除时先展示「将使多少令牌失效」的确认，确认后才真正提交。 */
async function handleSave() {
  if (removedGroups.value.length > 0 && revokeImpact.value === null) {
    const version = selectionVersion;
    impactLoading.value = true;
    try {
      const tokens = await apiClient.listUserTokens(props.user.id);
      if (version !== selectionVersion) return; // 期间选择又变：计数已过时
      const removed = new Set(removedGroups.value);
      revokeImpact.value = tokens.filter((token) => removed.has(token.model_group)).length;
    } catch (err) {
      if (version !== selectionVersion) return;
      // 拉不到令牌清单不阻塞流程，但绝不能按「0 把失效」展示——那会被读成
      // 没有影响。退到 unknown 文案，破坏性后果如实可见。
      error(extractApiError(err).message);
      revokeImpact.value = 'unknown';
    } finally {
      impactLoading.value = false;
    }
    return;
  }
  revokeImpact.value = null;
  saveMutation.mutate();
}
</script>

<template>
  <div>
    <div class="card-body space-y-3">
      <div
        v-if="unrestricted"
        class="bg-surface-elevated border-seed rounded-md border p-6 text-center space-y-2"
      >
        <span class="badge badge-neutral font-mono text-xs px-3 py-1">
          {{ t('users.groupsUnrestricted') }}
        </span>
        <p class="text-fg-muted text-xs max-w-sm mx-auto leading-relaxed">
          {{ t('users.groupsUnrestrictedHint') }}
        </p>
      </div>

      <template v-else>
        <InlineError
          v-if="groupsQuery.isError.value && !groupsQuery.data.value"
          :message="extractApiError(groupsQuery.error.value).message"
          @retry="() => groupsQuery.refetch()"
        />
        <template v-else>
          <fieldset class="border-seed border p-3">
            <legend class="text-fg-muted flex w-full items-center gap-1.5 px-1 text-xs font-medium">
              {{ t('users.groups') }}
              <span
                class="badge badge-neutral font-mono"
                data-testid="user-groups-count"
              >
                {{ selected.length }} / {{ allGroupNames.length }}
              </span>
              <span class="legend-rule" aria-hidden="true" />
              <button
                type="button"
                class="legend-btn"
                data-testid="user-groups-select-all"
                :disabled="allGroupNames.length === 0 || selected.length === allGroupNames.length"
                @click="selectAll"
              >
                {{ t('users.groupsSelectAll') }}
              </button>
              <button
                type="button"
                class="legend-btn text-danger border-danger/30 bg-danger-bg hover:border-danger hover:text-danger"
                data-testid="user-groups-clear-all"
                :disabled="selected.length === 0"
                @click="clearAll"
              >
                {{ t('users.groupsClearAll') }}
              </button>
            </legend>

            <div class="space-y-3">
              <div v-if="allGroupNames.length > 4" class="flex items-center gap-2">
                <SearchInput
                  id="user-groups-search"
                  v-model="filterText"
                  class="flex-1"
                  :placeholder="t('common.search')"
                  :aria-label="t('common.search')"
                />
              </div>

              <EmptyState v-if="filteredGroupNames.length === 0" :title="t('users.groupsEmpty')" />
              <div
                v-else
                class="seed-scrollbar max-h-64 overflow-y-auto pr-0.5"
              >
                <div class="grid grid-cols-1 gap-2 sm:grid-cols-2">
                  <label
                    v-for="name in filteredGroupNames"
                    :key="name"
                    :for="`user-group-check-${name}`"
                    class="border-seed hover:bg-surface-alt flex cursor-pointer items-center gap-2.5 rounded-md border p-2.5 transition-colors"
                    :class="{ 'bg-surface-elevated border-primary/40': isSelected(name) }"
                  >
                    <Checkbox
                      :id="`user-group-check-${name}`"
                      :model-value="isSelected(name)"
                      :data-testid="`user-group-${name}`"
                      @update:model-value="(checked: boolean) => toggleGroup(name, checked)"
                    />
                    <span class="truncate text-xs font-medium flex-1">
                      {{ groupDisplayName(name, t('models.ungrouped')) }}
                    </span>
                  </label>
                </div>
              </div>
            </div>
          </fieldset>
        </template>
      </template>
    </div>
    <div class="card-footer card-body flex justify-between gap-2">
      <button type="button" class="btn" @click="emit('close')">{{ t('common.cancel') }}</button>
      <button
        v-if="unrestricted"
        type="button"
        class="btn btn-primary"
        @click="emit('close')"
      >
        {{ t('common.confirm') }}
      </button>
      <!-- 撤组会使令牌立即失效：第一步展示影响，确认后才提交。 -->
      <div v-else-if="revokeImpact !== null" class="flex items-center gap-2">
        <p
          class="text-danger max-w-xs text-right text-xs leading-relaxed"
          data-testid="user-groups-revoke-warning"
        >
          {{
            revokeImpact === 'unknown'
              ? t('users.groupsRemoveUnknown', { groups: removedGroups.length })
              : t('users.groupsRemoveWarning', {
                  groups: removedGroups.length,
                  tokens: revokeImpact,
                })
          }}
        </p>
        <button
          type="button"
          class="btn"
          data-testid="user-groups-revoke-back"
          @click="revokeImpact = null"
        >
          {{ t('common.cancel') }}
        </button>
        <button
          type="button"
          class="btn btn-danger-filled"
          data-testid="user-groups-revoke-confirm"
          :disabled="saveMutation.isPending.value"
          @click="handleSave"
        >
          {{ t('common.confirm') }}
        </button>
      </div>
      <button
        v-else
        type="button"
        class="btn btn-primary"
        data-testid="user-groups-save"
        :disabled="saveMutation.isPending.value || groupsQuery.isPending.value || impactLoading"
        @click="handleSave"
      >
        {{ impactLoading ? t('common.loading') : t('common.save') }}
      </button>
    </div>
  </div>
</template>
