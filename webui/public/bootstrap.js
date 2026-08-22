// 首帧前的主题/语言引导：从 localStorage 读取偏好，避免暗色主题闪白。
// 必须外置为同源脚本：内联脚本会被 CSP `script-src 'self'` 拦截。
document.documentElement.lang = localStorage.getItem('kairos-locale') ?? 'zh-CN';
(function () {
  var stored = localStorage.getItem('kairos-theme');
  var isDark =
    stored === 'dark' ||
    (stored !== 'light' && window.matchMedia('(prefers-color-scheme: dark)').matches);
  if (isDark) {
    document.documentElement.classList.add('dark');
  }
})();
