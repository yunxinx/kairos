/** e2e 用的 micro-USD → `$` 展示，整数算术，与展示层约定一致。 */
export function usdLabel(micros: number): string {
  const abs = micros < 0 ? -micros : micros;
  const whole = Math.trunc(abs / 1_000_000);
  const frac = String(abs % 1_000_000)
    .padStart(6, '0')
    .replace(/0+$/, '');
  const amount = frac.length === 0 ? String(whole) : `${whole}.${frac}`;
  return micros < 0 ? `-$${amount}` : `$${amount}`;
}
