<script setup lang="ts">
import { ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { TabsContent, TabsIndicator, TabsList, TabsRoot, TabsTrigger } from 'reka-ui';
import type { UserAdminView } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import UserGroupsWindow from '@/features/users/UserGroupsWindow.vue';
import UserRechargeWindow from '@/features/users/UserRechargeWindow.vue';
import UserTokensWindow from '@/features/users/UserTokensWindow.vue';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

export type UserManageTab = 'recharge' | 'groups' | 'tokens';

const props = withDefaults(
  defineProps<{
    user: UserAdminView;
    tab: UserManageTab;
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
    cascade?: number;
    attention?: boolean;
    topmost?: boolean;
  }>(),
  { anchor: null, stackOrder: 0, cascade: 0, attention: false, topmost: true },
);

const emit = defineEmits<{
  close: [];
  raise: [];
  'dirty-change': [dirty: boolean];
}>();

const { t } = useI18n();
const activeTab = ref<UserManageTab>(props.tab);
const rechargeDirty = ref(false);
const groupsDirty = ref(false);

watch(
  () => props.tab,
  (tab) => {
    activeTab.value = tab;
  },
);

function emitDirty() {
  emit('dirty-change', rechargeDirty.value || groupsDirty.value);
}
</script>

<template>
  <FloatingWindow
    :title="t('users.manageTitle', { name: user.display_name })"
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    wide
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <TabsRoot v-model="activeTab">
      <div class="px-4 pt-3">
        <TabsList class="page-tab-switch" :aria-label="t('users.manageTabs')">
          <TabsIndicator class="page-tab-switch-knob" />
          <TabsTrigger value="recharge" class="page-tab-switch-btn" data-testid="user-tab-recharge">
            {{ t('users.recharge') }}
          </TabsTrigger>
          <TabsTrigger value="groups" class="page-tab-switch-btn" data-testid="user-tab-groups">
            {{ t('users.assignGroups') }}
          </TabsTrigger>
          <TabsTrigger value="tokens" class="page-tab-switch-btn" data-testid="user-tab-tokens">
            {{ t('users.viewTokens') }}
          </TabsTrigger>
        </TabsList>
      </div>
      <TabsContent value="recharge">
        <UserRechargeWindow
          :user="user"
          @close="emit('close')"
          @dirty-change="
            (dirty) => {
              rechargeDirty = dirty;
              emitDirty();
            }
          "
        />
      </TabsContent>
      <TabsContent value="groups">
        <UserGroupsWindow
          :user="user"
          @close="emit('close')"
          @dirty-change="
            (dirty) => {
              groupsDirty = dirty;
              emitDirty();
            }
          "
        />
      </TabsContent>
      <TabsContent value="tokens">
        <UserTokensWindow :user="user" />
      </TabsContent>
    </TabsRoot>
  </FloatingWindow>
</template>
