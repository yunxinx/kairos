import type { SystemLogEntry } from '@/api/types';
import { formatUsdMicros } from '@/lib/format';

type Translate = (key: string, values?: Record<string, unknown>) => string;
type HasTranslation = (key: string) => boolean;

/** 把事件参数转换为本地化模板可以直接展示的值。 */
function localizedParams(params: Record<string, unknown>, t: Translate): Record<string, unknown> {
  const result = { ...params };
  for (const [key, value] of Object.entries(result)) {
    if (key.endsWith('_usd_micros') && typeof value === 'number') {
      const amount = formatUsdMicros(value);
      result[key] = key.startsWith('delta_') && value > 0 ? `+${amount}` : amount;
    }
    if ((key === 'enabled' || key === 'before_enabled') && typeof value === 'boolean') {
      result[`${key}_label`] = t(value ? 'users.statusEnabled' : 'users.statusDisabled');
    }
    if (Array.isArray(value)) {
      const separator = key === 'changes' ? t('logs.eventValues.changeSeparator') : ' → ';
      result[`${key}_label`] = value.join(separator);
    }
    if (key === 'role' && typeof value === 'string') {
      const roleKey = `users.role${value.charAt(0).toUpperCase()}${value.slice(1)}`;
      result[key] = t(roleKey);
    }
  }
  result.archived_suffix = params.archived ? t('logs.eventValues.archivedSuffix') : '';
  return result;
}

/** 已知事件按当前语言渲染，旧日志、未知事件或非法参数回退到 message。 */
export function localizedSystemLogMessage(
  entry: SystemLogEntry,
  t: Translate,
  te: HasTranslation,
): string {
  const code = entry.event_code;
  const params = entry.event_params;
  if (!code || !params || typeof params !== 'object' || Array.isArray(params)) {
    return entry.message;
  }
  const key = `logs.events.${code}`;
  if (!te(key)) {
    return entry.message;
  }
  try {
    return t(key, localizedParams(params, t));
  } catch {
    return entry.message;
  }
}
