import { parseUsdToMicros } from '@/lib/format';
import { UINT_PATTERN } from '@/lib/uint-parse';

/** 单字段校验规则。 */
export type ValidationRule =
  | { kind: 'required' }
  | { kind: 'minLength'; min: number }
  | { kind: 'uint'; min?: number }
  | { kind: 'usd'; min?: number };

export type ValidationTranslate = (key: string, params?: Record<string, unknown>) => string;

export type FieldValidationSpec = {
  name: string;
  value: string | number;
  rules: ValidationRule[];
};

/** 对单个值按顺序执行规则，返回首条错误文案。 */
export function validateValue(
  value: string | number,
  rules: ValidationRule[],
  t: ValidationTranslate,
): string | undefined {
  // Vue 的 v-model 对 <input type="number"> 会把值强转成 number，因此 value 运行时可能是 number。
  const trimmed = String(value).trim();

  for (const rule of rules) {
    if (rule.kind === 'required' && !trimmed) {
      return t('validation.required');
    }
    if (rule.kind === 'minLength' && String(value).length < rule.min) {
      return t('validation.minLength', { min: rule.min });
    }
    if (rule.kind === 'uint') {
      // 空值视为可选；是否必填由前置 `required` 规则决定，避免把“清除限制”误判为非法。
      if (!trimmed) continue;
      const parsed = Number(trimmed);
      if (!UINT_PATTERN.test(trimmed) || !Number.isSafeInteger(parsed)) {
        return t('validation.uint');
      }
      if (rule.min !== undefined && parsed < rule.min) {
        return t('validation.uintMin', { min: rule.min });
      }
    }
    if (rule.kind === 'usd') {
      if (!trimmed) continue;
      const parsed = parseUsdToMicros(trimmed);
      if (parsed === null) {
        return t('validation.usd');
      }
      // `min` 与解析结果同为 micro-USD（调用方传 `0` 表示非负）。
      if (rule.min !== undefined && parsed < rule.min) {
        return t('validation.usdMin', { min: rule.min });
      }
    }
  }

  return undefined;
}

/** 按字段顺序返回首个错误，用于渐进式提示。 */
export function findFirstFieldError(
  specs: FieldValidationSpec[],
  t: ValidationTranslate,
): { name: string; message: string } | null {
  for (const spec of specs) {
    const message = validateValue(spec.value, spec.rules, t);
    if (message) {
      return { name: spec.name, message };
    }
  }
  return null;
}

/** 批量校验，返回字段名到错误文案的映射。 */
export function validateFields(
  specs: FieldValidationSpec[],
  t: ValidationTranslate,
): Record<string, string> {
  const errors: Record<string, string> = {};

  for (const spec of specs) {
    const message = validateValue(spec.value, spec.rules, t);
    if (message) {
      errors[spec.name] = message;
    }
  }

  return errors;
}
