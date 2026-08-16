/** UI 图标名；集中维护 stroke 图标，避免各组件内联 SVG 分叉。 */
export type IconName =
  | 'chevron-down'
  | 'chevron-up'
  | 'chevron-left'
  | 'chevron-right'
  | 'chevrons-left'
  | 'chevrons-right'
  | 'more-horizontal'
  | 'check'
  | 'minus'
  | 'plus'
  | 'plus-circle'
  | 'menu'
  | 'close'
  | 'sun'
  | 'moon'
  | 'globe'
  | 'log-out'
  | 'circle-alert'
  | 'lock'
  | 'lock-open'
  | 'calendar'
  | 'search'
  | 'copy'
  | 'pencil'
  | 'grip-vertical';

export type IconDef = {
  viewBox?: string;
  paths?: readonly string[];
  lines?: readonly { x1: number; y1: number; x2: number; y2: number }[];
  circles?: readonly { cx: number; cy: number; r: number }[];
  strokeWidth?: number;
  strokeLinecap?: 'round' | 'butt';
  strokeLinejoin?: 'round' | 'miter' | 'bevel';
};

export const iconDefs: Record<IconName, IconDef> = {
  'chevron-down': {
    paths: ['M6 9l6 6 6-6'],
  },
  'chevron-up': {
    paths: ['M18 15l-6-6-6 6'],
  },
  'chevron-left': {
    paths: ['M15 18l-6-6 6-6'],
  },
  'chevron-right': {
    paths: ['M9 18l6-6-6-6'],
  },
  /** 分页「第一页」。路径来自 Lucide「chevrons-left」(ISC)。 */
  'chevrons-left': {
    paths: ['M11 17l-5-5 5-5', 'M18 17l-5-5 5-5'],
  },
  /** 分页「最后一页」。路径来自 Lucide「chevrons-right」(ISC)。 */
  'chevrons-right': {
    paths: ['M13 7l5 5-5 5', 'M6 7l5 5-5 5'],
  },
  /** 行操作菜单。路径来自 Lucide「more-horizontal」(ISC)。 */
  'more-horizontal': {
    circles: [
      { cx: 12, cy: 12, r: 1 },
      { cx: 19, cy: 12, r: 1 },
      { cx: 5, cy: 12, r: 1 },
    ],
  },
  check: {
    paths: ['M20 6L9 17l-5-5'],
    strokeWidth: 2.5,
  },
  /** 半选态。路径来自 Lucide「minus」(ISC)。 */
  minus: {
    paths: ['M5 12h14'],
    strokeWidth: 2.5,
    strokeLinecap: 'round',
  },
  /** 步进增加。路径来自 Lucide「plus」(ISC)。 */
  plus: {
    paths: ['M5 12h14', 'M12 5v14'],
    strokeWidth: 2.5,
    strokeLinecap: 'round',
  },
  /** 筛选触发。路径来自 Lucide「plus-circle」(ISC)。 */
  'plus-circle': {
    circles: [{ cx: 12, cy: 12, r: 10 }],
    paths: ['M8 12h8', 'M12 8v8'],
    strokeWidth: 2,
    strokeLinecap: 'round',
    strokeLinejoin: 'round',
  },
  menu: {
    lines: [
      { x1: 6, y1: 8, x2: 18, y2: 8 },
      { x1: 6, y1: 12, x2: 18, y2: 12 },
      { x1: 6, y1: 16, x2: 18, y2: 16 },
    ],
    strokeWidth: 2.5,
    strokeLinecap: 'round',
  },
  close: {
    lines: [
      { x1: 7, y1: 7, x2: 17, y2: 17 },
      { x1: 17, y1: 7, x2: 7, y2: 17 },
    ],
    strokeWidth: 2.5,
    strokeLinecap: 'round',
  },
  sun: {
    circles: [{ cx: 12, cy: 12, r: 5 }],
    lines: [
      { x1: 12, y1: 1, x2: 12, y2: 3 },
      { x1: 12, y1: 21, x2: 12, y2: 23 },
      { x1: 4.22, y1: 4.22, x2: 5.64, y2: 5.64 },
      { x1: 18.36, y1: 5.64, x2: 19.78, y2: 4.22 },
      { x1: 4.22, y1: 19.78, x2: 5.64, y2: 18.36 },
      { x1: 18.36, y1: 18.36, x2: 19.78, y2: 19.78 },
      { x1: 1, y1: 12, x2: 3, y2: 12 },
      { x1: 21, y1: 12, x2: 23, y2: 12 },
    ],
  },
  moon: {
    paths: ['M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z'],
  },
  globe: {
    circles: [{ cx: 12, cy: 12, r: 10 }],
    paths: [
      'M2 12h20',
      'M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z',
    ],
  },
  'log-out': {
    paths: ['M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4', 'M16 17l5-5-5-5', 'M21 12H9'],
  },
  'circle-alert': {
    circles: [{ cx: 12, cy: 12, r: 10 }],
    paths: ['M12 8v4', 'M12 16h.01'],
    strokeWidth: 1.75,
    strokeLinecap: 'round',
  },
  /**
   * 密码已遮罩。路径来自 Lucide「lock」(ISC)，与 lock-open 共用锁体。
   * @see https://github.com/lucide-icons/lucide
   */
  lock: {
    paths: [
      'M5 11H19A2 2 0 0 1 21 13V20A2 2 0 0 1 19 22H5A2 2 0 0 1 3 20V13A2 2 0 0 1 5 11Z',
      'M7 11V7a5 5 0 0 1 10 0v4',
    ],
    strokeWidth: 2,
    strokeLinecap: 'round',
    strokeLinejoin: 'round',
  },
  /** 密码已明文。路径来自 Lucide「lock-open」(ISC)。 */
  'lock-open': {
    paths: [
      'M5 11H19A2 2 0 0 1 21 13V20A2 2 0 0 1 19 22H5A2 2 0 0 1 3 20V13A2 2 0 0 1 5 11Z',
      'M7 11V7a5 5 0 0 1 9.9-1',
    ],
    strokeWidth: 2,
    strokeLinecap: 'round',
    strokeLinejoin: 'round',
  },
  /** 日历（时间范围选择）。路径来自 Lucide「calendar」(ISC)。 */
  calendar: {
    paths: ['M5 4h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z', 'M3 10h18'],
    lines: [
      { x1: 8, y1: 2, x2: 8, y2: 6 },
      { x1: 16, y1: 2, x2: 16, y2: 6 },
    ],
    strokeWidth: 2,
    strokeLinecap: 'round',
    strokeLinejoin: 'round',
  },
  /** 搜索。路径来自 Lucide「search」(ISC)。 */
  search: {
    circles: [{ cx: 11, cy: 11, r: 8 }],
    paths: ['M21 21l-4.35-4.35'],
    strokeWidth: 2,
    strokeLinecap: 'round',
    strokeLinejoin: 'round',
  },
  /** 复制。路径来自 Lucide「copy」(ISC)，矩形档以闭合路径表达。 */
  copy: {
    paths: ['M8 8h14v14H8Z', 'M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2'],
    strokeWidth: 2,
    strokeLinecap: 'round',
    strokeLinejoin: 'round',
  },
  /** 编辑。路径来自 Lucide「pencil」(ISC)。 */
  pencil: {
    paths: ['M17 3a2.828 2.828 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5L17 3z'],
    strokeWidth: 2,
    strokeLinecap: 'round',
    strokeLinejoin: 'round',
  },
  /** 拖拽手柄。路径来自 Lucide「grip-vertical」(ISC)。 */
  'grip-vertical': {
    circles: [
      { cx: 9, cy: 5, r: 1 },
      { cx: 9, cy: 12, r: 1 },
      { cx: 9, cy: 19, r: 1 },
      { cx: 15, cy: 5, r: 1 },
      { cx: 15, cy: 12, r: 1 },
      { cx: 15, cy: 19, r: 1 },
    ],
  },
};
