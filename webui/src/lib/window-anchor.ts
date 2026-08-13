/** 浮窗锚点：打开瞬间的视口坐标，决定浮窗的初始位置。 */
export interface FloatingWindowAnchor {
  x: number;
  y: number;
}

/**
 * 从触发事件提取浮窗锚点。指针事件（含菜单项 pointerup，其 `detail` 恒为 0）
 * 总有真实坐标；键盘触发的点击（MouseEvent 且 `detail === 0`）没有坐标，
 * 返回 null，浮窗退化为居中打开。
 */
export function anchorFromEvent(event: Event): FloatingWindowAnchor | null {
  if (event instanceof PointerEvent) {
    return { x: event.clientX, y: event.clientY };
  }
  if (event instanceof MouseEvent && event.detail > 0) {
    return { x: event.clientX, y: event.clientY };
  }
  return null;
}
