import { ref, watch } from 'vue';

const NAV_AVATAR_KEY = 'kairos-show-nav-avatar';
const NAV_NAME_KEY = 'kairos-show-nav-name';

function readStoredNavAvatar(): boolean {
  const stored = localStorage.getItem(NAV_AVATAR_KEY);
  if (stored === null) return true;
  return stored === 'true';
}

function readStoredNavName(): boolean {
  const stored = localStorage.getItem(NAV_NAME_KEY);
  if (stored === null) return true;
  return stored === 'true';
}

const showNavAvatarState = ref<boolean>(readStoredNavAvatar());
const showNavNameState = ref<boolean>(readStoredNavName());

watch(showNavAvatarState, (val) => {
  localStorage.setItem(NAV_AVATAR_KEY, String(val));
});

watch(showNavNameState, (val) => {
  localStorage.setItem(NAV_NAME_KEY, String(val));
});

export function useNavAvatarPreference() {
  return {
    showNavAvatar: showNavAvatarState,
    setShowNavAvatar(val: boolean) {
      showNavAvatarState.value = val;
    },
  };
}

export function useNavNamePreference() {
  return {
    showNavName: showNavNameState,
    setShowNavName(val: boolean) {
      showNavNameState.value = val;
    },
  };
}
