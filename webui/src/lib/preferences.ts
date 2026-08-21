import { ref, watch } from 'vue';

const NAV_AVATAR_KEY = 'kairos-show-nav-avatar';

function readStoredNavAvatar(): boolean {
  const stored = localStorage.getItem(NAV_AVATAR_KEY);
  if (stored === null) return true;
  return stored === 'true';
}

const showNavAvatarState = ref<boolean>(readStoredNavAvatar());

watch(showNavAvatarState, (val) => {
  localStorage.setItem(NAV_AVATAR_KEY, String(val));
});

export function useNavAvatarPreference() {
  return {
    showNavAvatar: showNavAvatarState,
    setShowNavAvatar(val: boolean) {
      showNavAvatarState.value = val;
    },
  };
}
