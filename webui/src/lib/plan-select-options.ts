import type { PlanAudience, PlanView } from '@/api/types';
import type { ListboxSelectOption } from '@/lib/listbox-option';

/** 当前套餐的保留投影；用于套餐不再出现在当前受众名单时保持可见。 */
export interface CurrentPlanOption {
  value: string;
  label: string;
}

/**
 * 将套餐读视图投影为套餐选择器选项。
 *
 * 受众筛选、稳定排序、备注和默认徽章都集中在这里，避免不同用户编辑入口
 * 对同一组套餐产生不一致的显示结果。输入数组不会被原地修改。
 */
export function toPlanSelectOptions(
  plans: readonly PlanView[],
  audience: PlanAudience,
  defaultBadge: string,
  current?: CurrentPlanOption,
): ListboxSelectOption[] {
  const options = [...plans]
    .filter((plan) => plan.audience === audience)
    .sort((left, right) => left.display_name.localeCompare(right.display_name))
    .map((plan): ListboxSelectOption => ({
      value: String(plan.id),
      label: plan.display_name,
      ...(plan.note ? { description: plan.note } : {}),
      ...(plan.is_default ? { badge: defaultBadge } : {}),
    }));

  if (current && !options.some((option) => option.value === current.value)) {
    options.push(current);
  }

  return options;
}
