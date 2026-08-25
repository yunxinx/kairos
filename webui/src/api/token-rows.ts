import { apiClient } from '@/api/client';
import type { TokenView } from '@/api/types';
import { captureSessionGeneration, setMeForSession } from '@/lib/session';
import { tokenGroupUsable } from '@/lib/visible-models';

export type TokenRow = TokenView & {
  group_usable: boolean;
};

/**
 * 列表页与导航预取共用：令牌列表 + 当前用户身份。
 *
 * `getMe()` 不是为了钱包数字——令牌额度是每把令牌自己的 `limit_usd_micros`。这里取当前
 * 用户是为了 `setMe` 注水会话，并用其角色与可用组判定每把令牌绑定的组是否仍可调用。
 */
export async function loadTokenRows(): Promise<TokenRow[]> {
  const generation = captureSessionGeneration();
  const [listed, me] = await Promise.all([apiClient.listTokens(), apiClient.getMe()]);
  setMeForSession(me, generation);
  return listed.map((token) => ({
    ...token,
    group_usable: tokenGroupUsable(token.model_group, me.role, me.assigned_groups),
  }));
}
