<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { TabsContent, TabsIndicator, TabsList, TabsRoot, TabsTrigger } from 'reka-ui';
import type { UserAdminView } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import UserGroupsWindow from '@/features/users/UserGroupsWindow.vue';
import UserProfileTab from '@/features/users/UserProfileTab.vue';
import UserRechargeWindow from '@/features/users/UserRechargeWindow.vue';
import UserTokensWindow from '@/features/users/UserTokensWindow.vue';
import { useCurrentUser } from '@/lib/session';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

export type UserManageTab = 'profile' | 'recharge' | 'groups' | 'tokens';

const props = withDefaults(
  defineProps<{
    user: UserAdminView;
    tab?: UserManageTab;
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
    cascade?: number;
    attention?: boolean;
    topmost?: boolean;
  }>(),
  { tab: 'profile', anchor: null, stackOrder: 0, cascade: 0, attention: false, topmost: true },
);

const emit = defineEmits<{
  close: [];
  raise: [];
  'dirty-change': [dirty: boolean];
}>();

const { t } = useI18n();
const me = useCurrentUser();
const isSelf = computed(() => me.value !== null && me.value.id === props.user.id);

const initialTab = computed<UserManageTab>(() => {
  if (isSelf.value && props.tab === 'tokens') return 'profile';
  return props.tab;
});

const activeTab = ref<UserManageTab>(initialTab.value);
const profileDirty = ref(false);
const rechargeDirty = ref(false);
const groupsDirty = ref(false);

const windowTitle = computed(() => {
  const name = props.user.display_name || props.user.email;
  const mail = props.user.email;
  return name && name !== mail ? `${name} (${mail})` : mail;
});

watch(
  () => props.tab,
  (tab) => {
    activeTab.value = isSelf.value && tab === 'tokens' ? 'profile' : tab;
  },
);

function emitDirty() {
  emit('dirty-change', profileDirty.value || rechargeDirty.value || groupsDirty.value);
}
</script>

<template>
  <FloatingWindow
    :title="windowTitle"
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
          <TabsTrigger value="profile" class="page-tab-switch-btn" data-testid="user-tab-profile">
            {{ t('users.profile') }}
          </TabsTrigger>
          <TabsTrigger value="recharge" class="page-tab-switch-btn" data-testid="user-tab-recharge">
            {{ t('users.recharge') }}
          </TabsTrigger>
          <TabsTrigger value="groups" class="page-tab-switch-btn" data-testid="user-tab-groups">
            {{ t('users.assignGroups') }}
          </TabsTrigger>
          <TabsTrigger
            v-if="!isSelf"
            value="tokens"
            class="page-tab-switch-btn"
            data-testid="user-tab-tokens"
          >
            {{ t('users.viewTokens') }}
          </TabsTrigger>
        </TabsList>
      </div>
      <TabsContent value="profile">
        <UserProfileTab
          :user="user"
          @close="emit('close')"
          @dirty-change="
            (dirty) => {
              profileDirty = dirty;
              emitDirty();
            }
          "
        />
      </TabsContent>
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
      <TabsContent v-if="!isSelf" value="tokens">
        <UserTokensWindow :user="user" />
      </TabsContent>
    </TabsRoot>
  </FloatingWindow>
</template>
