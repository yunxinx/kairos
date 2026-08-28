<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { TabsContent, TabsIndicator, TabsList, TabsRoot, TabsTrigger } from 'reka-ui';
import type { UserAdminView } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import UserProfileTab from '@/features/users/UserProfileTab.vue';
import UserRechargeWindow from '@/features/users/UserRechargeWindow.vue';
import UserTokensWindow from '@/features/users/UserTokensWindow.vue';
import { hasCapability } from '@/lib/capabilities';
import { useCurrentUser } from '@/lib/session';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

export type UserManageTab = 'profile' | 'recharge' | 'tokens';

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
const canToggleTokens = computed(() => hasCapability(me.value, 'toggle_user_tokens'));

const initialTab = computed<UserManageTab>(() => {
  if (props.tab === 'tokens' && (isSelf.value || !canToggleTokens.value)) return 'profile';
  return props.tab;
});

const activeTab = ref<UserManageTab>(initialTab.value);
const profileDirty = ref(false);
const rechargeDirty = ref(false);
const rechargeBusy = ref(false);

const windowTitle = computed(() => {
  const name = props.user.display_name || props.user.email;
  const mail = props.user.email;
  return name && name !== mail ? `${name} (${mail})` : mail;
});

watch(
  () => props.tab,
  () => {
    activeTab.value = initialTab.value;
  },
);

function emitDirty() {
  emit('dirty-change', profileDirty.value || rechargeDirty.value);
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
    :close-disabled="rechargeBusy"
    wide
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <TabsRoot v-model="activeTab">
      <div class="px-4 pt-3">
        <TabsList class="page-tab-switch" :aria-label="t('users.manageTabs')">
          <TabsIndicator class="page-tab-switch-knob" />
          <TabsTrigger
            value="profile"
            class="page-tab-switch-btn"
            data-testid="user-tab-profile"
            :disabled="rechargeBusy"
          >
            {{ t('users.profile') }}
          </TabsTrigger>
          <TabsTrigger
            value="recharge"
            class="page-tab-switch-btn"
            data-testid="user-tab-recharge"
            :disabled="rechargeBusy"
          >
            {{ t('users.recharge') }}
          </TabsTrigger>
          <TabsTrigger
            v-if="!isSelf && canToggleTokens"
            value="tokens"
            class="page-tab-switch-btn"
            data-testid="user-tab-tokens"
            :disabled="rechargeBusy"
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
          @busy-change="rechargeBusy = $event"
          @dirty-change="
            (dirty) => {
              rechargeDirty = dirty;
              emitDirty();
            }
          "
        />
      </TabsContent>
      <TabsContent v-if="!isSelf && canToggleTokens" value="tokens">
        <UserTokensWindow :user="user" />
      </TabsContent>
    </TabsRoot>
  </FloatingWindow>
</template>
