export type ThemeMode = 'light' | 'dark' | 'system';

const THEME_KEY = 'kairos-theme';

function isTheme(value: string | null): value is ThemeMode {
  return value === 'light' || value === 'dark' || value === 'system';
}

export function getStoredTheme(): ThemeMode {
  const stored = localStorage.getItem(THEME_KEY);
  return isTheme(stored) ? stored : 'system';
}

export function resolveDark(theme: ThemeMode): boolean {
  if (theme === 'system') {
    return window.matchMedia('(prefers-color-scheme: dark)').matches;
  }
  return theme === 'dark';
}

export function applyTheme(theme: ThemeMode): boolean {
  const isDark = resolveDark(theme);
  document.documentElement.classList.toggle('dark', isDark);
  return isDark;
}

export function setTheme(theme: ThemeMode): void {
  if (theme === 'system') localStorage.removeItem(THEME_KEY);
  else localStorage.setItem(THEME_KEY, theme);
  applyTheme(theme);
}

export function toggleTheme(): ThemeMode {
  const current = getStoredTheme();
  const isDark = resolveDark(current);
  const next: ThemeMode = isDark ? 'light' : 'dark';
  setTheme(next);
  return next;
}

export function initTheme(): boolean {
  return applyTheme(getStoredTheme());
}
