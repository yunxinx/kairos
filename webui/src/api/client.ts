import { invalidateAdminKey, getAdminKey } from '@/lib/session';
import { ApiClientError, type ApiErrorBody } from '@/api/types';
import type {
  BalanceAdjustment,
  BalanceView,
  Channel,
  ChannelProbeResult,
  ChannelView,
  LogQuery,
  Page,
  LogEntry,
  Price,
  Settings,
  StatsView,
  LifetimeStats,
  Token,
  TokenCreate,
  TokenView,
} from '@/api/types';

function buildQuery(params: object): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === '') continue;
    if (typeof value !== 'string' && typeof value !== 'number') continue;
    search.set(key, String(value));
  }
  const query = search.toString();
  return query ? `?${query}` : '';
}

/**
 * 调用管理 API。路径无 `/api` 前缀；认证为 `Authorization: Bearer <admin key>`。
 *
 * `keyOverride` 用于登录试探：失败时不清除已持有的凭据。
 */
async function apiFetch<T>(path: string, init?: RequestInit, keyOverride?: string): Promise<T> {
  const key = keyOverride ?? getAdminKey();
  const headers = new Headers(init?.headers);
  if (init?.body !== undefined && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json');
  }
  if (key) {
    headers.set('Authorization', `Bearer ${key}`);
  }

  const response = await fetch(path, { ...init, headers });
  if (!response.ok) {
    if (response.status === 401 && key && keyOverride === undefined) {
      invalidateAdminKey();
    }
    const body = (await response.json().catch(() => ({}))) as ApiErrorBody;
    const message = body.error?.message ?? response.statusText;
    throw new ApiClientError(message, body.error?.code);
  }
  if (response.status === 204) {
    return undefined as T;
  }
  return (await response.json()) as T;
}

export const apiClient = {
  listTokens(keyOverride?: string): Promise<TokenView[]> {
    return apiFetch('/tokens', { method: 'GET' }, keyOverride);
  },

  createToken(body: TokenCreate): Promise<TokenView> {
    return apiFetch('/tokens', { method: 'POST', body: JSON.stringify(body) });
  },

  updateToken(tokenKey: string, body: Token): Promise<TokenView> {
    return apiFetch(`/tokens/${encodeURIComponent(tokenKey)}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    });
  },

  deleteToken(tokenKey: string): Promise<TokenView> {
    return apiFetch(`/tokens/${encodeURIComponent(tokenKey)}`, { method: 'DELETE' });
  },

  adjustTokenBalance(tokenKey: string, body: BalanceAdjustment): Promise<BalanceView> {
    return apiFetch(`/tokens/${encodeURIComponent(tokenKey)}/balance`, {
      method: 'POST',
      body: JSON.stringify(body),
    });
  },

  /**
   * 读令牌余额：管理 API 无独立 GET，相对调整 `delta = 0` 返回当前余额且不改账。
   */
  readTokenBalance(tokenKey: string): Promise<BalanceView> {
    return apiFetch(`/tokens/${encodeURIComponent(tokenKey)}/balance`, {
      method: 'POST',
      body: JSON.stringify({ delta_usd_micros: 0 }),
    });
  },

  listChannels(): Promise<ChannelView[]> {
    return apiFetch('/channels');
  },

  createChannel(body: Channel): Promise<ChannelView> {
    return apiFetch('/channels', { method: 'POST', body: JSON.stringify(body) });
  },

  updateChannel(id: number, body: Channel): Promise<ChannelView> {
    return apiFetch(`/channels/${id}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    });
  },

  deleteChannel(id: number): Promise<ChannelView> {
    return apiFetch(`/channels/${id}`, { method: 'DELETE' });
  },

  testChannel(id: number): Promise<ChannelProbeResult> {
    return apiFetch(`/channels/${id}/test`, { method: 'POST' });
  },

  listPrices(): Promise<Price[]> {
    return apiFetch('/prices');
  },

  createPrice(body: Price): Promise<Price> {
    return apiFetch('/prices', { method: 'POST', body: JSON.stringify(body) });
  },

  updatePrice(model: string, body: Price): Promise<Price> {
    return apiFetch(`/prices/${encodeURIComponent(model)}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    });
  },

  deletePrice(model: string): Promise<Price> {
    return apiFetch(`/prices/${encodeURIComponent(model)}`, { method: 'DELETE' });
  },

  getSettings(): Promise<Settings> {
    return apiFetch('/settings');
  },

  updateSettings(body: Settings): Promise<Settings> {
    return apiFetch('/settings', { method: 'PUT', body: JSON.stringify(body) });
  },

  queryLogs(query: LogQuery = {}): Promise<Page<LogEntry>> {
    return apiFetch(`/logs${buildQuery(query)}`);
  },

  getStats(days?: number): Promise<StatsView> {
    return apiFetch(`/stats${buildQuery({ days })}`);
  },

  getLifetimeStats(): Promise<LifetimeStats> {
    return apiFetch('/stats/lifetime');
  },
};

export function extractApiError(err: unknown): { message: string; code?: string } {
  if (err instanceof ApiClientError) {
    return err.code === undefined
      ? { message: err.message }
      : { message: err.message, code: err.code };
  }
  if (err instanceof Error) return { message: err.message };
  return { message: 'Unknown error' };
}
