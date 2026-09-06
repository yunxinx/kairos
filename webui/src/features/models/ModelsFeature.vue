<script setup lang="ts">
import { computed, ref, watch } from 'vue';
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
import { hasCapability, type ManagementCapability } from '@/lib/capabilities';

const { t } = useI18n();
const me = useCurrentUser();

// 普通用户走另一条渲染路径：整页只有「我能用哪些模型」这一张只读表，不出标签页。
// 不把它做成第六个标签页——那会暗示还有别的标签页存在但被藏了。
const isPlainUser = computed(() => me.value?.role === 'user');

type ModelTabValue = 'inventory' | 'unified' | 'groups' | 'order' | 'visible';

interface ModelTabDefinition {
  value: ModelTabValue;
  labelKey: string;
  testId: string;
  capabilities: readonly ManagementCapability[];
}

/**
 * 每个标签列出它实际挂载的查询所需能力。只有全部能力都满足时才创建标签和内容，
 * 这样受限管理员不会先触发一个必然返回 403 的请求，再把错误状态渲染成空表。
 */
const tabDefinitions: readonly ModelTabDefinition[] = [
  {
    value: 'inventory',
    labelKey: 'models.tabInventory',
    testId: 'models-tab-inventory',
    capabilities: ['view_channels', 'view_prices'],
  },
  {
    value: 'unified',
    labelKey: 'models.tabUnified',
    testId: 'models-tab-unified',
    capabilities: ['view_unified_models', 'view_channels', 'view_prices'],
  },
  {
    value: 'groups',
    labelKey: 'models.tabGroups',
    testId: 'models-tab-groups',
    capabilities: ['view_model_groups', 'view_unified_models', 'view_channels'],
  },
  {
    value: 'order',
    labelKey: 'models.tabOrder',
    testId: 'models-tab-order',
    capabilities: ['view_channels'],
  },
  {
    value: 'visible',
    labelKey: 'models.tabVisible',
    testId: 'models-tab-visible',
    capabilities: ['view_model_groups', 'view_unified_models', 'view_channels'],
  },
];

const authorizedTabs = computed(() =>
  tabDefinitions.filter((tab) =>
    tab.capabilities.every((capability) => hasCapability(me.value, capability)),
  ),
);

const activeTab = ref<ModelTabValue>('inventory');

// 会话 hydrate 或套餐能力变更后，当前标签可能变成不可见；立即切到第一个仍可用的标签。
watch(
  authorizedTabs,
  (tabs) => {
    if (!tabs.some((tab) => tab.value === activeTab.value)) {
      activeTab.value = tabs[0]?.value ?? 'inventory';
    }
  },
  { immediate: true },
);

const canViewInventory = computed(() =>
  authorizedTabs.value.some((tab) => tab.value === 'inventory'),
);
const canViewUnified = computed(() => authorizedTabs.value.some((tab) => tab.value === 'unified'));
const canViewGroups = computed(() => authorizedTabs.value.some((tab) => tab.value === 'groups'));
const canViewOrder = computed(() => authorizedTabs.value.some((tab) => tab.value === 'order'));
const canViewVisible = computed(() => authorizedTabs.value.some((tab) => tab.value === 'visible'));
</script>

<template>
  <div v-if="isPlainUser" class="flex flex-col">
    <PageHeader :title="t('nav.models')" />
    <MyModelsPanel />
  </div>
  <TabsRoot
    v-else-if="authorizedTabs.length > 0"
    v-model="activeTab"
    class="flex flex-col"
  >
    <PageHeader>
      <template #leading>
        <TabsList class="page-tab-switch" :aria-label="t('models.tabsLabel')">
          <TabsIndicator class="page-tab-switch-knob" />
          <TabsTrigger
            v-for="tab in authorizedTabs"
            :key="tab.value"
            :value="tab.value"
            class="page-tab-switch-btn"
            :data-testid="tab.testId"
          >
            {{ t(tab.labelKey) }}
          </TabsTrigger>
        </TabsList>
      </template>
    </PageHeader>
    <TabsContent v-if="canViewInventory" value="inventory">
      <InventoryTab />
    </TabsContent>
    <TabsContent v-if="canViewUnified" value="unified">
      <UnifiedTab />
    </TabsContent>
    <TabsContent v-if="canViewGroups" value="groups">
      <GroupsTab />
    </TabsContent>
    <TabsContent v-if="canViewOrder" value="order">
      <OrderTab />
    </TabsContent>
    <TabsContent v-if="canViewVisible" value="visible">
      <VisibleTab />
    </TabsContent>
  </TabsRoot>
  <PageHeader v-else :title="t('nav.models')" />
</template>
