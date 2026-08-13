/** micro-USD 整数 → 美元字符串，六位小数对应全部微元，去尾零不丢精度。 */
export function formatUsdMicros(micros: number): string {
  const negative = micros < 0;
  const abs = negative ? -micros : micros;
  const dollars = Math.trunc(abs / 1_000_000);
  const fraction = (abs % 1_000_000).toString().padStart(6, '0').replace(/0+$/, '');
  const amount = fraction.length === 0 ? String(dollars) : `${dollars}.${fraction}`;
  return `${negative ? '-' : ''}$${amount}`;
}
