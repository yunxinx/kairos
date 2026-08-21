import { invalidateAdminKey, getAdminKey } from '@/lib/session';
import { ApiClientError, type ApiErrorBody } from '@/api/types';
import type {
  AssignedGroupsView,
  BalanceAdjustment,
  Channel,
  ChannelProbeResult,
  ChannelView,
  LogQuery,
  LogPage,
  LogEntry,
  SystemLogQuery,
  SystemLogPage,
  LoginRequest,
  LoginView,
  MeUpdate,
  MeView,
  ModelGroup,
  Price,
  CatalogModel,
  CatalogView,
  CatalogMeta,
  CatalogQuery,
  Settings,
  StatsView,
  LifetimeStats,
  Token,
  TokenCreate,
  TokenView,
  UnifiedModel,
  UpstreamModelsDraft,
  UpstreamModelsView,
  UserAdminView,
  UserCreate,
  UserUpdate,
  UserView,
} from '@/api/types';

function buildQuery(params: object): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === '') continue;
    if (Array.isArray(value)) {
      const joined = value.filter((item) => item !== undefined && item !== '').join(',');
      if (joined) search.set(key, joined);
      continue;
    }
    if (typeof value === 'boolean') {
      search.set(key, value ? 'true' : 'false');
      continue;
    }
    if (typeof value !== 'string' && typeof value !== 'number') continue;
    search.set(key, String(value));
  }
  const query = search.toString();
  return query ? `?${query}` : '';
}

/**
 * 调用管理 API。路径无 `/api` 前缀；认证为 `Authorization: Bearer <会话令牌>`。
 * 会话来自 `POST /login`，不是配置里的登录密码。
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
  login(body: LoginRequest): Promise<LoginView> {
    return apiFetch('/login', { method: 'POST', body: JSON.stringify(body) }, '');
  },

  logout(): Promise<void> {
    return apiFetch('/logout', { method: 'POST' });
  },

  getMe(): Promise<MeView> {
    return apiFetch('/me');
  },

  updateMe(body: MeUpdate): Promise<UserView> {
    return apiFetch('/me', { method: 'PUT', body: JSON.stringify(body) });
  },

  listUsers(): Promise<UserAdminView[]> {
    return apiFetch('/users');
  },

  getUser(id: number): Promise<UserAdminView> {
    return apiFetch(`/users/${id}`);
  },

  createUser(body: UserCreate): Promise<UserView> {
    return apiFetch('/users', { method: 'POST', body: JSON.stringify(body) });
  },

  updateUser(id: number, body: UserUpdate): Promise<UserView> {
    return apiFetch(`/users/${id}`, { method: 'PUT', body: JSON.stringify(body) });
  },

  deleteUser(id: number): Promise<void> {
    return apiFetch(`/users/${id}`, { method: 'DELETE' });
  },

  rechargeUser(id: number, body: BalanceAdjustment): Promise<UserAdminView> {
    return apiFetch(`/users/${id}/balance`, { method: 'POST', body: JSON.stringify(body) });
  },

  getUserModelGroups(id: number): Promise<AssignedGroupsView> {
    return apiFetch(`/users/${id}/model-groups`);
  },

  replaceUserModelGroups(id: number, groups: string[]): Promise<AssignedGroupsView> {
    return apiFetch(`/users/${id}/model-groups`, {
      method: 'PUT',
      body: JSON.stringify({ groups }),
    });
  },

  listUserTokens(id: number): Promise<TokenView[]> {
    return apiFetch(`/users/${id}/tokens`);
  },

  listTokens(keyOverride?: string): Promise<TokenView[]> {
    return apiFetch('/tokens', { method: 'GET' }, keyOverride);
  },

  createToken(body: TokenCreate): Promise<TokenView> {
    return apiFetch('/tokens', { method: 'POST', body: JSON.stringify(body) });
  },

  updateToken(id: number, body: Token): Promise<TokenView> {
    return apiFetch(`/tokens/${id}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    });
  },

  deleteToken(id: number): Promise<TokenView> {
    return apiFetch(`/tokens/${id}`, { method: 'DELETE' });
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

  testChannel(id: number, model: string): Promise<ChannelProbeResult> {
    return apiFetch(`/channels/${id}/test`, {
      method: 'POST',
      body: JSON.stringify({ model }),
    });
  },

  /** 按渠道草稿拉取上游模型列表；渠道无需已保存。 */
  listUpstreamModels(body: UpstreamModelsDraft): Promise<UpstreamModelsView> {
    return apiFetch('/channels/models', { method: 'POST', body: JSON.stringify(body) });
  },

  listPrices(): Promise<Price[]> {
    return apiFetch('/prices');
  },

  createPrice(body: Price): Promise<Price> {
    return apiFetch('/prices', { method: 'POST', body: JSON.stringify(body) });
  },

  updatePrice(channelId: number, model: string, body: Price): Promise<Price> {
    return apiFetch(`/prices/${channelId}/${encodeURIComponent(model)}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    });
  },

  deletePrice(channelId: number, model: string): Promise<Price> {
    return apiFetch(`/prices/${channelId}/${encodeURIComponent(model)}`, { method: 'DELETE' });
  },

  listModelGroups(): Promise<ModelGroup[]> {
    return apiFetch('/model-groups');
  },

  createModelGroup(body: ModelGroup): Promise<ModelGroup> {
    return apiFetch('/model-groups', { method: 'POST', body: JSON.stringify(body) });
  },

  updateModelGroup(name: string, body: ModelGroup): Promise<ModelGroup> {
    return apiFetch(`/model-groups/${encodeURIComponent(name)}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    });
  },

  deleteModelGroup(name: string): Promise<ModelGroup> {
    return apiFetch(`/model-groups/${encodeURIComponent(name)}`, { method: 'DELETE' });
  },

  listUnifiedModels(): Promise<UnifiedModel[]> {
    return apiFetch('/unified-models');
  },

  createUnifiedModel(body: UnifiedModel): Promise<UnifiedModel> {
    return apiFetch('/unified-models', { method: 'POST', body: JSON.stringify(body) });
  },

  updateUnifiedModel(id: string, body: UnifiedModel): Promise<UnifiedModel> {
    return apiFetch(`/unified-models/${encodeURIComponent(id)}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    });
  },

  deleteUnifiedModel(id: string): Promise<UnifiedModel> {
    return apiFetch(`/unified-models/${encodeURIComponent(id)}`, { method: 'DELETE' });
  },

  getSettings(): Promise<Settings> {
    return apiFetch('/settings');
  },

  updateSettings(body: Settings): Promise<Settings> {
    return apiFetch('/settings', { method: 'PUT', body: JSON.stringify(body) });
  },

  getCatalog(params?: CatalogQuery): Promise<CatalogView> {
    if (!params) return apiFetch('/catalog');
    return apiFetch(`/catalog${buildQuery(params)}`);
  },

  getCatalogMeta(): Promise<CatalogMeta> {
    return apiFetch('/catalog/meta');
  },

  replaceCatalog(models: CatalogModel[]): Promise<CatalogView> {
    return apiFetch('/catalog', { method: 'PUT', body: JSON.stringify({ models }) });
  },

  syncCatalog(): Promise<CatalogView> {
    return apiFetch('/catalog/sync', { method: 'POST' });
  },

  queryLogs(query: LogQuery = {}): Promise<LogPage> {
    return apiFetch(`/logs${buildQuery(query)}`);
  },

  getLog(id: number): Promise<LogEntry> {
    return apiFetch(`/logs/${id}`);
  },

  settleLog(id: number): Promise<LogEntry> {
    return apiFetch(`/logs/${id}/settle`, { method: 'POST' });
  },

  waiveLog(id: number): Promise<LogEntry> {
    return apiFetch(`/logs/${id}/waive`, { method: 'POST' });
  },

  querySystemLogs(query: SystemLogQuery = {}): Promise<SystemLogPage> {
    return apiFetch(`/system-logs${buildQuery(query)}`);
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
