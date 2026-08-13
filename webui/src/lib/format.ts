const MICROS_PER_USD = 1_000_000;
const USD_PATTERN = /^(-?)(\d+)(?:\.(\d{1,6}))?$/;

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
