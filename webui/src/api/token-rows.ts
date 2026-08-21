import { apiClient } from '@/api/client';
import type { TokenView } from '@/api/types';
import { setMe } from '@/lib/session';
import { tokenGroupUsable } from '@/lib/visible-models';

export type TokenRow = TokenView & {
  balance_usd_micros: number;
  group_usable: boolean;
};

/** 列表页与导航预取共用：令牌列表 + 当前用户钱包（多令牌共用额度）。 */
export async function loadTokenRows(): Promise<TokenRow[]> {
  const [listed, me] = await Promise.all([apiClient.listTokens(), apiClient.getMe()]);
  setMe(me);
  return listed.map((token) => ({
    ...token,
    balance_usd_micros: me.balance_usd_micros,
    group_usable: tokenGroupUsable(token.model_group, me.role, me.assigned_groups),
  }));
}
