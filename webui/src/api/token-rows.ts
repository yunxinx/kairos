import { apiClient } from '@/api/client';
import type { BalanceView, TokenView } from '@/api/types';

export type TokenRow = TokenView & {
  balance_usd_micros: number;
  settled_usd_micros: number;
};

/** 列表页与导航预取共用：先列令牌，再串行读余额（避免并发写锁）。 */
export async function loadTokenRows(): Promise<TokenRow[]> {
  const listed = await apiClient.listTokens();
  const balances: BalanceView[] = [];
  for (const token of listed) {
    balances.push(await apiClient.readTokenBalance(token.token_key));
  }
  const byKey = new Map(balances.map((item) => [item.token_key, item]));
  return listed.map((token) => {
    const balance = byKey.get(token.token_key);
    return {
      ...token,
      balance_usd_micros: balance?.balance_usd_micros ?? 0,
      settled_usd_micros: balance?.settled_usd_micros ?? 0,
    };
  });
}
