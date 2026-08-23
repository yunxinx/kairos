<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { TabsContent, TabsIndicator, TabsList, TabsRoot, TabsTrigger } from 'reka-ui';
import PageHeader from '@/app/layout/PageHeader.vue';
import { hasCapability } from '@/lib/capabilities';
import { useCurrentUser } from '@/lib/session';
import GroupsTab from '@/features/models/GroupsTab.vue';
import InventoryTab from '@/features/models/InventoryTab.vue';
import UnifiedTab from '@/features/models/UnifiedTab.vue';
import VisibleTab from '@/features/models/VisibleTab.vue';

const { t } = useI18n();
const me = useCurrentUser();

const canInventory = computed(() => {
  if (me.value?.role === 'root') return true;
  return hasCapability(me.value, 'edit_prices') || hasCapability(me.value, 'edit_price_catalog');
});
const canUnified = computed(() => {
  if (me.value?.role === 'root') return true;
  return hasCapability(me.value, 'edit_unified_models');
});
const canGroups = computed(() => {
  if (me.value?.role === 'root') return true;
  return (
    hasCapability(me.value, 'edit_model_groups') ||
    hasCapability(me.value, 'view_own_plan_groups') ||
    hasCapability(me.value, 'view_other_groups')
  );
});
const canVisible = computed(() => {
  if (me.value?.role === 'root') return true;
  return (
    hasCapability(me.value, 'view_own_plan_groups') ||
    hasCapability(me.value, 'view_other_groups')
  );
});

const defaultTab = computed(() => {
  if (canInventory.value) return 'inventory';
  if (canUnified.value) return 'unified';
  if (canGroups.value) return 'groups';
  return 'visible';
});
</script>

<template>
  <TabsRoot :default-value="defaultTab" class="flex flex-col">
    <PageHeader>
      <template #leading>
        <TabsList class="page-tab-switch" :aria-label="t('models.tabsLabel')">
          <TabsIndicator class="page-tab-switch-knob" />
          <TabsTrigger
            v-if="canInventory"
            value="inventory"
            class="page-tab-switch-btn"
            data-testid="models-tab-inventory"
          >
            {{ t('models.tabInventory') }}
          </TabsTrigger>
          <TabsTrigger v-if="canUnified" value="unified" class="page-tab-switch-btn" data-testid="models-tab-unified">
            {{ t('models.tabUnified') }}
          </TabsTrigger>
          <TabsTrigger v-if="canGroups" value="groups" class="page-tab-switch-btn" data-testid="models-tab-groups">
            {{ t('models.tabGroups') }}
          </TabsTrigger>
          <TabsTrigger v-if="canVisible" value="visible" class="page-tab-switch-btn" data-testid="models-tab-visible">
            {{ t('models.tabVisible') }}
          </TabsTrigger>
        </TabsList>
      </template>
    </PageHeader>
    <TabsContent v-if="canInventory" value="inventory">
      <InventoryTab />
    </TabsContent>
    <TabsContent v-if="canUnified" value="unified">
      <UnifiedTab />
    </TabsContent>
    <TabsContent v-if="canGroups" value="groups">
      <GroupsTab />
    </TabsContent>
    <TabsContent v-if="canVisible" value="visible">
      <VisibleTab />
    </TabsContent>
  </TabsRoot>
</template>
