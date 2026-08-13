import { createI18n } from 'vue-i18n';
import en from '@/locales/en.json';
import zhCN from '@/locales/zh-CN.json';

export const i18n = createI18n({
  legacy: false,
  locale: localStorage.getItem('kairos-locale') ?? 'zh-CN',
  fallbackLocale: 'en',
  messages: {
    en,
    'zh-CN': zhCN,
  },
});

/** 与 vue-i18n 同步，供标题/衬线字体栈切换（html[lang]） */
export function syncDocumentLocale(locale: string): void {
  document.documentElement.lang = locale;
}

export function setLocale(locale: 'en' | 'zh-CN'): void {
  i18n.global.locale.value = locale;
  localStorage.setItem('kairos-locale', locale);
  syncDocumentLocale(locale);
}

export function toggleLocale(): 'en' | 'zh-CN' {
  const next = i18n.global.locale.value === 'zh-CN' ? 'en' : 'zh-CN';
  setLocale(next);
  return next;
}
