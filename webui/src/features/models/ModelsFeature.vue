<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { TabsContent, TabsIndicator, TabsList, TabsRoot, TabsTrigger } from 'reka-ui';
import PageHeader from '@/app/layout/PageHeader.vue';
import GroupsTab from '@/features/models/GroupsTab.vue';
import InventoryTab from '@/features/models/InventoryTab.vue';
import UnifiedTab from '@/features/models/UnifiedTab.vue';
import VisibleTab from '@/features/models/VisibleTab.vue';

const { t } = useI18n();
</script>

<template>
  <TabsRoot default-value="inventory" class="flex flex-col">
    <PageHeader>
      <template #leading>
        <TabsList class="page-tab-switch" :aria-label="t('models.tabsLabel')">
          <TabsIndicator class="page-tab-switch-knob" />
          <TabsTrigger
            value="inventory"
            class="page-tab-switch-btn"
            data-testid="models-tab-inventory"
          >
            {{ t('models.tabInventory') }}
          </TabsTrigger>
          <TabsTrigger value="unified" class="page-tab-switch-btn" data-testid="models-tab-unified">
            {{ t('models.tabUnified') }}
          </TabsTrigger>
          <TabsTrigger value="groups" class="page-tab-switch-btn" data-testid="models-tab-groups">
            {{ t('models.tabGroups') }}
          </TabsTrigger>
          <TabsTrigger value="visible" class="page-tab-switch-btn" data-testid="models-tab-visible">
            {{ t('models.tabVisible') }}
          </TabsTrigger>
        </TabsList>
      </template>
    </PageHeader>
    <TabsContent value="inventory">
      <InventoryTab />
    </TabsContent>
    <TabsContent value="unified">
      <UnifiedTab />
    </TabsContent>
    <TabsContent value="groups">
      <GroupsTab />
    </TabsContent>
    <TabsContent value="visible">
      <VisibleTab />
    </TabsContent>
  </TabsRoot>
</template>
