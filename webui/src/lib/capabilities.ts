import type { MeView, PlanCapabilities } from '@/api/types';

/** 全部管理面能力键，与后端 `PlanCapabilities` 字段一致。 */
export const PLAN_CAPABILITY_KEYS: readonly (keyof PlanCapabilities)[] = [
  'manage_users',
  'assign_plan',
  'view_logs_stats',
  'settle_waive',
  'toggle_user_tokens',
  'view_own_plan_groups',
  'view_other_groups',
  'edit_prices',
  'edit_model_groups',
  'edit_unified_models',
  'edit_price_catalog',
];

/** 新管理员内置档默认打开的能力集合。 */
export const DEFAULT_ADMIN_CAPABILITIES: PlanCapabilities = {
  manage_users: true,
  assign_plan: true,
  view_logs_stats: true,
  settle_waive: true,
  toggle_user_tokens: true,
  view_own_plan_groups: true,
  view_other_groups: false,
  edit_prices: false,
  edit_model_groups: false,
  edit_unified_models: false,
  edit_price_catalog: false,
};

/** 全空能力集合，用于新建自定义套餐表单初始值。 */
export const EMPTY_CAPABILITIES: PlanCapabilities = {
  manage_users: false,
  assign_plan: false,
  view_logs_stats: false,
  settle_waive: false,
  toggle_user_tokens: false,
  view_own_plan_groups: false,
  view_other_groups: false,
  edit_prices: false,
  edit_model_groups: false,
  edit_unified_models: false,
  edit_price_catalog: false,
};

export type ManagementCapability = keyof PlanCapabilities;

/** root 不受套餐开关约束；admin 取套餐开关；user 不具备管理面能力。 */
export function hasCapability(me: MeView | null | undefined, capability: ManagementCapability): boolean {
  if (!me) return false;
  if (me.role === 'root') return true;
  if (me.role === 'admin') return me.capabilities[capability];
  return false;
}

/** 管理面导航/页内动作使用的别名：只有 root 能访问 root-only 区域。 */
export function isRoot(me: MeView | null | undefined): boolean {
  return me?.role === 'root';
}
