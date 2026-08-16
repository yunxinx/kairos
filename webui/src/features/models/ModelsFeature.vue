<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { TabsContent, TabsList, TabsRoot, TabsTrigger } from 'reka-ui';
import PageHeader from '@/app/layout/PageHeader.vue';
import GroupsTab from '@/features/models/GroupsTab.vue';
import InventoryTab from '@/features/models/InventoryTab.vue';
import UnifiedTab from '@/features/models/UnifiedTab.vue';
import VisibleTab from '@/features/models/VisibleTab.vue';

const { t } = useI18n();
</script>

<template>
  <div class="flex flex-col">
    <PageHeader :title="t('nav.models')" />
    <TabsRoot default-value="inventory">
      <TabsList class="page-tabs-list" :aria-label="t('models.tabsLabel')">
        <TabsTrigger value="inventory" class="page-tabs-trigger" data-testid="models-tab-inventory">
          {{ t('models.tabInventory') }}
        </TabsTrigger>
        <TabsTrigger value="unified" class="page-tabs-trigger" data-testid="models-tab-unified">
          {{ t('models.tabUnified') }}
        </TabsTrigger>
        <TabsTrigger value="groups" class="page-tabs-trigger" data-testid="models-tab-groups">
          {{ t('models.tabGroups') }}
        </TabsTrigger>
        <TabsTrigger value="visible" class="page-tabs-trigger" data-testid="models-tab-visible">
          {{ t('models.tabVisible') }}
        </TabsTrigger>
      </TabsList>
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
  </div>
</template>
