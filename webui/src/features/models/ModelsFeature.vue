<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { TabsContent, TabsIndicator, TabsList, TabsRoot, TabsTrigger } from 'reka-ui';
import PageHeader from '@/app/layout/PageHeader.vue';
import { useCurrentUser } from '@/lib/session';
import GroupsTab from '@/features/models/GroupsTab.vue';
import InventoryTab from '@/features/models/InventoryTab.vue';
import MyModelsPanel from '@/features/models/MyModelsPanel.vue';
import OrderTab from '@/features/models/OrderTab.vue';
import UnifiedTab from '@/features/models/UnifiedTab.vue';
import VisibleTab from '@/features/models/VisibleTab.vue';

const { t } = useI18n();
const me = useCurrentUser();

// 普通用户走另一条渲染路径：整页只有「我能用哪些模型」这一张只读表，不出标签页。
// 不把它做成第六个标签页——那会暗示还有别的标签页存在但被藏了。
const isPlainUser = computed(() => me.value?.role === 'user');

const defaultTab = 'inventory';
</script>

<template>
  <div v-if="isPlainUser" class="flex flex-col">
    <PageHeader :title="t('nav.models')" />
    <MyModelsPanel />
  </div>
  <TabsRoot v-else :default-value="defaultTab" class="flex flex-col">
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
          <TabsTrigger value="order" class="page-tab-switch-btn" data-testid="models-tab-order">
            {{ t('models.tabOrder') }}
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
    <TabsContent value="order">
      <OrderTab />
    </TabsContent>
    <TabsContent value="visible">
      <VisibleTab />
    </TabsContent>
  </TabsRoot>
</template>
