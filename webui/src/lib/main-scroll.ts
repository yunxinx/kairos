/**
 * 把页面级滚动容器（AppShell 的 main）瞬时滚回顶部。
 * 表格自然撑开后，翻页停留在页面底部，需要带回表格起点继续浏览。
 */
export function scrollMainToTop(): void {
  const main = document.getElementById('main-content');
  if (main) {
    main.scrollTop = 0;
  }
}
