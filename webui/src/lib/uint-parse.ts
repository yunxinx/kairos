/** 纯数字（无符号整数）字符串模式；校验与解析共用，避免 `parseInt` 截断 `12abc`/`12.5`。 */
export const UINT_PATTERN = /^\d+$/;

/**
 * 严格解析可选的无符号整数值。
 *
 * 接受 `string | number`：Vue 的 `v-model` 对 `<input type="number">` 会自动把值强转成 number，
 * 因此 settings 里数字字段的 ref 运行时可能是 number。这里统一先 `String(value)` 再校验。
 *
 * - 空字符串 → `null`（语义为“清除该限制”）。
 * - 纯数字且在 `Number.isSafeInteger` 范围 → 对应数值。
 * - 其他（`12abc`、`12.5`、负数、超大溢出）→ `null`。
 *
 * 仅用于校验通过后的值→数字转换；表单合法性应由 `uint` 校验规则把关，
 * 不要用 `Number.parseInt` 作为表单校验——它会静默截断 `12.5` → `12`，
 * 把用户输错的内容保存成另一个真实配置。
 */
export function parseOptionalUint(value: string | number): number | null {
  const trimmed = String(value).trim();
  if (!trimmed) return null;
  if (!UINT_PATTERN.test(trimmed)) return null;
  const parsed = Number(trimmed);
  if (!Number.isSafeInteger(parsed) || parsed < 0) return null;
  return parsed;
}
