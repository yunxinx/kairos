const MICROS_PER_USD = 1_000_000;
const USD_PATTERN = /^(-?)(\d+)(?:\.(\d{1,6}))?$/;

function resolveNumberLocale(locale?: string): string | undefined {
  if (locale === 'zh-CN') return 'zh-CN';
  if (locale === 'en') return 'en-US';
  return undefined;
}

/** micro-USD 整数 → 不含 `$` 的美元字符串，六位小数对应全部微元，去尾零不丢精度。 */
export function formatUsdAmount(micros: number): string {
  const negative = micros < 0;
  const abs = negative ? -micros : micros;
  const dollars = Math.trunc(abs / MICROS_PER_USD);
  const fraction = (abs % MICROS_PER_USD).toString().padStart(6, '0').replace(/0+$/, '');
  const amount = fraction.length === 0 ? String(dollars) : `${dollars}.${fraction}`;
  return `${negative ? '-' : ''}${amount}`;
}

/** micro-USD 整数 → 带 `$` 的美元字符串，展示层用。 */
export function formatUsdMicros(micros: number): string {
  const amount = formatUsdAmount(micros);
  if (amount.startsWith('-')) {
    return `-$${amount.slice(1)}`;
  }
  return `$${amount}`;
}

/**
 * 美元可读字符串 → micro-USD 整数。
 *
 * 接受可选 `$` 前缀、最多六位小数；空串返回 `null`。非法格式或超出安全整数返回 `null`。
 * 用整数算术换算，避免 `Number` 浮点丢微元。
 */
export function parseUsdToMicros(input: string): number | null {
  const trimmed = input.trim().replace(/^\$/, '');
  if (!trimmed) {
    return null;
  }
  const match = USD_PATTERN.exec(trimmed);
  if (!match) {
    return null;
  }
  const negative = match[1] === '-';
  const dollars = Number(match[2]);
  const fraction = (match[3] ?? '').padEnd(6, '0');
  if (!Number.isSafeInteger(dollars)) {
    return null;
  }
  const micros = dollars * MICROS_PER_USD + Number(fraction);
  if (!Number.isSafeInteger(micros)) {
    return null;
  }
  return negative ? -micros : micros;
}

const MICROS_PER_MILLION_TOKENS = 1_000_000n;

/** 对齐后端 `billing::component_cost`：tokens × 单价 / 1M，向零截断。 */
export function componentCostMicros(tokens: number, microsPer1m: number): number {
  const tokenCount = Math.trunc(tokens);
  const price = Math.trunc(microsPer1m);
  if (!Number.isSafeInteger(tokenCount) || !Number.isSafeInteger(price)) {
    return 0;
  }
  return Number((BigInt(tokenCount) * BigInt(price)) / MICROS_PER_MILLION_TOKENS);
}

/** unix 毫秒 → 本地化日期时间。 */
export function formatUnixMillis(millis: number, locale?: string): string {
  return new Date(millis).toLocaleString(resolveNumberLocale(locale));
}

/** 掩码中段后仍保留明文的最小长度：前 8 + 掩码 + 后 8。 */
const TOKEN_KEY_VISIBLE_EDGE = 8;
const TOKEN_KEY_MASK = '******';

/** 令牌 key 掩码展示：前、后各保留 8 位明文，中间固定以六个 `*` 代替。 */
export function maskTokenKey(key: string): string {
  if (key.length <= TOKEN_KEY_VISIBLE_EDGE * 2) {
    return key;
  }
  return `${key.slice(0, TOKEN_KEY_VISIBLE_EDGE)}${TOKEN_KEY_MASK}${key.slice(-TOKEN_KEY_VISIBLE_EDGE)}`;
}

const MILLIS_PER_SECOND = 1_000;
const DAYS_PER_MONTH = 30;
const DAYS_PER_YEAR = 365;

/** 相对时间的分档结果，展示层据此选择文案模板。 */
export type RelativeTimeParts =
  | { kind: 'seconds'; seconds: number }
  | { kind: 'minutesSeconds'; minutes: number; seconds: number }
  | { kind: 'hoursMinutes'; hours: number; minutes: number }
  | { kind: 'daysHours'; days: number; hours: number }
  | { kind: 'monthsDays'; months: number; days: number }
  | { kind: 'yearsMonthsDays'; years: number; months: number; days: number };

/**
 * 时间差（毫秒）→ 相对时间分档。
 *
 * 分档规则：<1 分钟显示秒；<1 小时显示「分 秒」；<1 天显示「时 分」；
 * <1 月（按 30 天）显示「天 时」；<1 年（按 365 天）显示「月 天」；
 * 达到年单位显示「年 月 天」。月/年为近似换算。
 */
export function relativeTimeParts(deltaMillis: number): RelativeTimeParts {
  const totalSeconds = Math.max(0, Math.floor(deltaMillis / MILLIS_PER_SECOND));
  if (totalSeconds < 60) {
    return { kind: 'seconds', seconds: totalSeconds };
  }
  const totalMinutes = Math.floor(totalSeconds / 60);
  if (totalMinutes < 60) {
    return { kind: 'minutesSeconds', minutes: totalMinutes, seconds: totalSeconds % 60 };
  }
  const totalHours = Math.floor(totalMinutes / 60);
  if (totalHours < 24) {
    return { kind: 'hoursMinutes', hours: totalHours, minutes: totalMinutes % 60 };
  }
  const totalDays = Math.floor(totalHours / 24);
  if (totalDays < DAYS_PER_MONTH) {
    return { kind: 'daysHours', days: totalDays, hours: totalHours % 24 };
  }
  if (totalDays < DAYS_PER_YEAR) {
    return {
      kind: 'monthsDays',
      months: Math.floor(totalDays / DAYS_PER_MONTH),
      days: totalDays % DAYS_PER_MONTH,
    };
  }
  const years = Math.floor(totalDays / DAYS_PER_YEAR);
  const remainderDays = totalDays - years * DAYS_PER_YEAR;
  return {
    kind: 'yearsMonthsDays',
    years,
    months: Math.floor(remainderDays / DAYS_PER_MONTH),
    days: remainderDays % DAYS_PER_MONTH,
  };
}

/** 整数计数 → 本地化千分位，展示层用。 */
export function formatCount(value: number, locale?: string): string {
  return new Intl.NumberFormat(resolveNumberLocale(locale)).format(value);
}

/** 0–1 比例 → 百分比字符串（最多一位小数）。 */
export function formatPercent(ratio: number, locale?: string): string {
  return new Intl.NumberFormat(resolveNumberLocale(locale), {
    style: 'percent',
    maximumFractionDigits: 1,
  }).format(ratio);
}

const TOKENS_PER_MILLION = 1_000_000;
/** 与网关 `DEFAULT_MAX_REQUEST_BYTES = 100 * 1024 * 1024` 同一档（1 MB = 1 MiB）。 */
const BYTES_PER_MB = 1024 * 1024;
const MB_PATTERN = /^(\d+)(?:\.(\d{1,2}))?$/;

/** 总 token 数 → 百万单位，固定四位小数，后缀 ` M`。 */
export function formatTokensMillions(tokens: number): string {
  return `${(tokens / TOKENS_PER_MILLION).toFixed(4)} M`;
}

/** 字节 → MB 字符串，固定两位小数。 */
export function formatBytesAsMb(bytes: number): string {
  return (bytes / BYTES_PER_MB).toFixed(2);
}

/**
 * MB 可读字符串 → 字节整数。
 *
 * 接受最多两位小数；`0.01` 档不是整字节，四舍五入。空串或非法格式返回 `null`。
 */
export function parseMbToBytes(input: string): number | null {
  const trimmed = input.trim();
  if (!trimmed) {
    return null;
  }
  const match = MB_PATTERN.exec(trimmed);
  if (!match) {
    return null;
  }
  const whole = Number(match[1]);
  const fraction = (match[2] ?? '').padEnd(2, '0');
  if (!Number.isSafeInteger(whole)) {
    return null;
  }
  const hundredths = whole * 100 + Number(fraction);
  const bytes = Math.round((hundredths * BYTES_PER_MB) / 100);
  if (!Number.isSafeInteger(bytes)) {
    return null;
  }
  return bytes;
}

/** 探测延迟展示：不足 1s 用整数毫秒，达到 1s 进位为秒（两位小数）。 */
export function formatProbeLatency(ms: number): string {
  if (ms >= 1_000) {
    return `${(ms / 1_000).toFixed(2)} s`;
  }
  return `${ms} ms`;
}

/** Token 计数精简展示（例如 1.2k, 500, 1.5M）。 */
export function formatTokensCount(tokens: number): string {
  if (tokens < 1_000) {
    return String(tokens);
  }
  if (tokens < 1_000_000) {
    const k = (tokens / 1_000).toFixed(1).replace(/\.0$/, '');
    return `${k}k`;
  }
  const m = (tokens / 1_000_000)
    .toFixed(2)
    .replace(/\.00$/, '')
    .replace(/(\.\d)0$/, '$1');
  return `${m}M`;
}

/** Token 生成速度（Tokens/s）。 */
export function formatThroughput(outputTokens: number, latencyMs: number): string | null {
  if (outputTokens <= 0 || latencyMs <= 0) {
    return null;
  }
  const seconds = latencyMs / 1_000;
  const speed = outputTokens / seconds;
  return `${speed >= 100 ? speed.toFixed(0) : speed.toFixed(1)} t/s`;
}

/** 日志耗时展示：不足 1s 用整数毫秒，达到 1s 用一位小数秒。 */
export function formatLatencyDisplay(ms: number): string {
  if (ms < MILLIS_PER_SECOND) {
    return `${ms}ms`;
  }
  return `${(ms / MILLIS_PER_SECOND).toFixed(1)}s`;
}

export type LatencyHealth = 'excellent' | 'normal' | 'slow';

/** 预填充/排队预算：对齐「首字」5s 绿 / 10s 黄的绝对档。 */
const PREFILL_EXCELLENT_MS = 5_000;
const PREFILL_NORMAL_MS = 10_000;
/** 生成速度基准：达到该吞吐则整段耗时仍算健康。 */
const TPS_EXCELLENT = 30;
const TPS_NORMAL = 10;

/**
 * 评估耗时健康度。
 *
 * 日志没有独立的首字耗时，色带只能标整段墙钟时间。预算 = 预填充档 + 按输出量估算的生成时间：
 * 输出越多允许越久，同时仍受生成速度约束。无输出时退化为 5s / 10s 绝对档。
 */
export function evaluateLatencyHealth(latencyMs: number, outputTokens: number): LatencyHealth {
  const generationExcellentMs =
    outputTokens > 0 ? (outputTokens / TPS_EXCELLENT) * MILLIS_PER_SECOND : 0;
  const generationNormalMs = outputTokens > 0 ? (outputTokens / TPS_NORMAL) * MILLIS_PER_SECOND : 0;
  if (latencyMs <= PREFILL_EXCELLENT_MS + generationExcellentMs) return 'excellent';
  if (latencyMs <= PREFILL_NORMAL_MS + generationNormalMs) return 'normal';
  return 'slow';
}

/** 计算缓存命中率（百分比）。 */
export function computeCacheHitRatio(readTokens: number, inputTokens: number): number {
  const total = inputTokens + readTokens;
  if (total <= 0 || readTokens <= 0) return 0;
  return Math.round((readTokens / total) * 100);
}
