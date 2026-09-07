import { ref, watch } from 'vue';

const NAV_AVATAR_KEY = 'kairos-show-nav-avatar';
const NAV_NAME_KEY = 'kairos-show-nav-name';

/**
 * 读一个数字偏好：值必须是白名单成员，否则回落默认值（不写回，等下次变更自然覆盖）。
 * localStorage 的读写对禁用/隐私窗口可能抛异常，按「无偏好」处理。
 */
export function readValidatedNumber(
  key: string,
  allowed: readonly number[],
  fallback: number,
): number {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    const parsed = Number.parseInt(raw, 10);
    return allowed.includes(parsed) ? parsed : fallback;
  } catch {
    return fallback;
  }
}

/** 写一个数字偏好；失败（存储被禁等）静默放弃，功能不因持久化不可用而中断。 */
export function writePlainNumber(key: string, value: number): void {
  try {
    localStorage.setItem(key, String(value));
  } catch {
    // 持久化是锦上添花：写不进去就当会话内状态用。
  }
}

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
