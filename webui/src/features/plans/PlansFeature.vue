<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { PlanView } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import InlineError from '@/components/ui/InlineError.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableMenuItem from '@/components/ui/data-table/DataTableMenuItem.vue';
import DataTableRowActions from '@/components/ui/data-table/DataTableRowActions.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { useWindowStack } from '@/composables/useWindowStack';
import { useToast } from '@/composables/useToast';
import PlanEditorWindow from '@/features/plans/PlanEditorWindow.vue';
import { formatDiscountBp } from '@/lib/format';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type PlanWindowPayload =
  | { kind: 'editor'; plan: PlanView | null }
  | { kind: 'delete'; plan: PlanView };

const { t } = useI18n();
const { error } = useToast();
const queryClient = useQueryClient();
const pendingAnchor = ref<FloatingWindowAnchor | null>(null);

function takePendingAnchor(): FloatingWindowAnchor | null {
  const anchor = pendingAnchor.value;
  pendingAnchor.value = null;
  return anchor;
}

const {
  windows,
  topmostId,
  open: openWindow,
  close: closeWindow,
  setDirty,
  bringToFront,
} = useWindowStack<PlanWindowPayload>();

const plansQuery = useQuery({
  queryKey: ['plans'],
  queryFn: () => apiClient.listPlans(),
});

const plans = computed(() => plansQuery.data.value ?? []);
const showTableSkeleton = computed(() => plansQuery.isPending.value && !plansQuery.data.value);

const deleteErrors = ref<Record<number, string>>({});
const deletingId = ref<number | null>(null);

const deleteMutation = useMutation({
  mutationFn: async (plan: PlanView) => {
    deletingId.value = plan.id;
    await apiClient.deletePlan(plan.id, true);
  },
  onSuccess: async (_data, plan) => {
    const entry = windows.value.find(
      (win) => win.payload.kind === 'delete' && win.payload.plan.id === plan.id,
    );
    if (entry) closeWindow(entry.id);
    await queryClient.invalidateQueries({ queryKey: ['plans'] });
  },
  onError: (err, plan) => {
    const entry = windows.value.find(
      (win) => win.payload.kind === 'delete' && win.payload.plan.id === plan.id,
    );
    if (entry) deleteErrors.value[entry.id] = extractApiError(err).message;
    else error(extractApiError(err).message);
  },
  onSettled: () => {
    deletingId.value = null;
  },
});

function openCreate(event: Event) {
  openWindow(anchorFromEvent(event), { kind: 'editor', plan: null });
}

function openEdit(plan: PlanView) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'editor' && entry.payload.plan?.id === plan.id,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'editor', plan });
}

function openDelete(plan: PlanView) {
  if (plan.builtin) return;
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'delete' && entry.payload.plan.id === plan.id,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete', plan });
  if (entry) deleteErrors.value[entry.id] = '';
}

watch(plans, (rows) => {
  for (const entry of windows.value) {
    const payload = entry.payload;
    const planId = payload.kind === 'editor' ? payload.plan?.id : payload.plan.id;
    const latest = rows.find((plan) => plan.id === planId);
    if (!latest && payload.kind === 'delete') continue;
    if (!latest && payload.kind === 'editor') closeWindow(entry.id);
    else if (latest && payload.kind === 'editor') payload.plan = latest;
  }
});
</script>

<template>
  <div class="flex flex-col">
    <PageHeader :title="t('nav.plans')" />

    <InlineError
      v-if="plansQuery.isError.value && !plansQuery.data.value"
      :message="extractApiError(plansQuery.error.value).message"
      @retry="() => plansQuery.refetch()"
    />

    <div v-else class="flex flex-col">
      <DataTable :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <template #actions>
              <button
                type="button"
                class="btn btn-primary"
                data-testid="create-plan"
                @click="openCreate"
              >
                {{ t('plans.create') }}
              </button>
            </template>
          </DataTableToolbar>
        </template>
        <TableHeader>
          <TableRow>
            <TableHead>{{ t('plans.internalName') }}</TableHead>
            <TableHead>{{ t('plans.displayName') }}</TableHead>
            <TableHead>{{ t('plans.discount') }}</TableHead>
            <TableHead>{{ t('plans.defaultRpm') }}</TableHead>
            <TableHead>{{ t('plans.sharedRpm') }}</TableHead>
            <TableHead>{{ t('plans.groupsCount') }}</TableHead>
            <TableHead>{{ t('plans.share') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="8" />
          <template v-else>
            <TableRow
              v-for="plan in plans"
              :key="plan.id"
              data-testid="plan-row"
              :data-plan-id="String(plan.id)"
            >
              <TableCell class="font-mono" data-testid="plan-internal">
                <span class="inline-flex items-center gap-1.5">
                  {{ plan.internal_name }}
                  <span v-if="plan.builtin" class="badge badge-neutral text-[10px]">
                    {{ t('plans.builtin') }}
                  </span>
                </span>
              </TableCell>
              <TableCell data-testid="plan-display">{{ plan.display_name }}</TableCell>
              <TableCell class="font-mono" data-testid="plan-discount-cell">
                {{ formatDiscountBp(plan.discount_bp) }}
              </TableCell>
              <TableCell class="font-mono">
                <span v-if="plan.default_rpm">{{ plan.default_rpm }}</span>
                <span v-else class="text-fg-muted">-</span>
              </TableCell>
              <TableCell class="font-mono">
                <span v-if="plan.shared_rpm">{{ plan.shared_rpm }}</span>
                <span v-else class="text-fg-muted">-</span>
              </TableCell>
              <TableCell class="font-mono">{{ plan.groups.length }}</TableCell>
              <TableCell>
                <span
                  v-if="plan.shared_with_admin"
                  class="badge badge-success"
                  data-testid="plan-shared-badge"
                >
                  {{ t('common.yes') }}
                </span>
                <span v-else class="text-fg-muted">{{ t('common.no') }}</span>
              </TableCell>
              <TableCell align="center">
                <span class="inline-flex items-center gap-1">
                  <button
                    type="button"
                    class="btn btn-ghost btn-icon"
                    data-testid="plan-edit"
                    :aria-label="t('common.edit')"
                    :title="t('common.edit')"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @click="openEdit(plan)"
                  >
                    <UiIcon name="pencil" :size="16" />
                  </button>
                  <DataTableRowActions v-if="!plan.builtin">
                    <DataTableMenuItem
                      danger
                      data-testid="plan-delete"
                      @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                      @select="openDelete(plan)"
                    >
                      {{ t('common.delete') }}
                    </DataTableMenuItem>
                  </DataTableRowActions>
                </span>
              </TableCell>
            </TableRow>
            <TableRow v-if="plans.length === 0">
              <TableCell :colspan="8" class="h-24 whitespace-normal">
                <EmptyState :title="t('plans.empty')">
                  <button type="button" class="btn btn-primary" @click="openCreate">
                    {{ t('plans.create') }}
                  </button>
                </EmptyState>
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
    </div>

    <template v-for="(win, index) in windows" :key="win.id">
      <PlanEditorWindow
        v-if="win.payload.kind === 'editor'"
        :initial="win.payload.plan"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
      />
      <ConfirmWindow
        v-else
        :title="t('plans.deleteTitle')"
        :message="t('plans.deleteMessage', { name: win.payload.plan.display_name })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="deletingId === win.payload.plan.id"
        confirm-test-id="plan-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="deleteMutation.mutate(win.payload.plan)"
      />
    </template>
  </div>
</template>
