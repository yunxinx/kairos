/** 时间范围（unix 毫秒）；端点为 `null` 表示该端不限。 */
export interface DateRange {
  from: number | null;
  to: number | null;
}

const MS_PER_DAY = 86_400_000;

function pad(value: number): string {
  return String(value).padStart(2, '0');
}

/** unix 毫秒 → `datetime-local` 输入值（本地时区，精确到分钟）。 */
export function millisToDatetimeLocal(millis: number): string {
  const date = new Date(millis);
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** `datetime-local` 输入值 → unix 毫秒；空串或非法值返回 `null`。 */
export function datetimeLocalToMillis(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) {
    return null;
  }
  const millis = new Date(trimmed).getTime();
  return Number.isFinite(millis) ? millis : null;
}

/** unix 毫秒 → `YYYY-MM-DD HH:mm` 展示文本（本地时区）。 */
export function formatDatetimeShort(millis: number): string {
  const date = new Date(millis);
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 范围展示文本：未设置的端点以省略号占位；两端均未设置返回空串。 */
export function formatRangeLabel(range: DateRange): string {
  if (range.from === null && range.to === null) {
    return '';
  }
  const from = range.from === null ? '…' : formatDatetimeShort(range.from);
  const to = range.to === null ? '…' : formatDatetimeShort(range.to);
  return `${from} ~ ${to}`;
}

/** 本地时区当天 00:00 的 unix 毫秒。 */
export function startOfToday(now: Date): number {
  return new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
}

/** 本地时区当月 1 日 00:00 的 unix 毫秒。 */
export function startOfMonth(now: Date): number {
  return new Date(now.getFullYear(), now.getMonth(), 1).getTime();
}

/** 快速选择项标识。 */
export type DateRangeQuickKey = 'today' | 'days7' | 'days15' | 'days30' | 'month';

/**
 * 快速选择 → 范围（本地时区）：`今天`/`N 天` 取包含今天在内的整日窗口
 * （如 7 天 = 今天往前 6 个整日 00:00 起），`本月` 取当月 1 日起；结束均为当前时刻。
 */
export function quickRange(key: DateRangeQuickKey, now: Date): DateRange {
  const to = now.getTime();
  const today = startOfToday(now);
  switch (key) {
    case 'today':
      return { from: today, to };
    case 'days7':
      return { from: today - 6 * MS_PER_DAY, to };
    case 'days15':
      return { from: today - 14 * MS_PER_DAY, to };
    case 'days30':
      return { from: today - 29 * MS_PER_DAY, to };
    case 'month':
      return { from: startOfMonth(now), to };
  }
}
