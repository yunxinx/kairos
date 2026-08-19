/** 渠道 wire 协议，与后端 `Protocol` serde rename 一致。 */
export type Protocol = 'openai_chat' | 'openai_responses' | 'anthropic_messages';

/** 出站路径段，与网关 `protocol::upstream_path` 对齐。 */
const UPSTREAM_PATH: Record<Protocol, string> = {
  openai_chat: '/chat/completions',
  openai_responses: '/responses',
  anthropic_messages: '/messages',
};

/** 渠道出站 URL：去掉 base_url 尾斜杠后接协议路径。 */
export function channelOutboundUrl(protocol: Protocol, baseUrl: string): string {
  return `${baseUrl.replace(/\/+$/, '')}${UPSTREAM_PATH[protocol]}`;
}

/** 令牌写契约；余额与生命周期元数据不在其中。 */
export interface Token {
  token_key: string;
  name: string;
  limit_usd_micros: number | null;
  /** 每分钟请求上限；`null` 跟随全局兜底，`0` 表示该令牌不限速。 */
  rate_limit_rpm: number | null;
  enabled: boolean;
  /** 绑定的模型组名；未指定时为内置 `default`。 */
  model_group: string;
}

/** 令牌创建契约：不接受指定 key，key 由系统生成并随响应返回。 */
export type TokenCreate = Omit<Token, 'token_key'>;

/** 令牌读响应：写契约字段 + 生命周期元数据。 */
export interface TokenView extends Token {
  /** 创建时刻（unix 毫秒）。 */
  created_at: number;
  /** 最后使用时刻（unix 毫秒）；null 表示从未使用。 */
  last_used_at: number | null;
}

/** 渠道：出站接入单元（写契约，不含库生成身份）。 */
export interface Channel {
  name: string;
  protocol: Protocol;
  base_url: string;
  api_key: string;
  models: string[];
  model_aliases: Record<string, string>;
  priority: number;
  weight: number;
  timeout_ms: number;
  max_retries: number;
  /** 是否启用：禁用的渠道不参与路由与失败切换。 */
  enabled: boolean;
  /** 保存时把新加入的可调用名并入该组；`default` 表示不自动入组。 */
  model_group: string;
}

/** 渠道读响应：库生成的稳定身份 + 写契约字段。 */
export interface ChannelView extends Channel {
  id: number;
}

/** 读视图 → 写契约：剥离只读 id（后端 `deny_unknown_fields` 拒收未知字段）。 */
export function channelWriteBody(view: ChannelView): Channel {
  return {
    name: view.name,
    protocol: view.protocol,
    base_url: view.base_url,
    api_key: view.api_key,
    models: view.models,
    model_aliases: view.model_aliases,
    priority: view.priority,
    weight: view.weight,
    timeout_ms: view.timeout_ms,
    max_retries: view.max_retries,
    enabled: view.enabled,
    model_group: view.model_group,
  };
}

/** 某一渠道上某一已登记模型名的四档单价（micro-USD / 1M tokens）。 */
export interface Price {
  channel_id: number;
  model: string;
  input_micros: number;
  output_micros: number;
  cache_read_micros: number | null;
  cache_write_micros: number | null;
}

/** 组名单一条：钉渠道的已登记名，或统一模型 ID。 */
export type GroupModel =
  | { kind: 'source'; channel_id: number; model: string }
  | { kind: 'unified'; id: string };

/** 模型组：令牌的可调用名允许名单。 */
export interface ModelGroup {
  name: string;
  models: GroupModel[];
}

/** 统一模型的一条成员：钉在某一渠道上的已登记可调用名。 */
export interface UnifiedMember {
  channel_id: number;
  model: string;
  /** GET 读视图：渠道仍在、已启用且仍登记该名。写契约不含此字段。 */
  available?: boolean;
}

/** 写契约：剥离只读 `available`（后端 `deny_unknown_fields` 拒收未知字段）。 */
export function unifiedMemberWriteBody(
  member: UnifiedMember,
): Pick<UnifiedMember, 'channel_id' | 'model'> {
  return { channel_id: member.channel_id, model: member.model };
}

/** 统一模型：一个下游可调用名，按顺序尝试若干钉渠道的成员。 */
export interface UnifiedModel {
  id: string;
  models: UnifiedMember[];
  hide: boolean;
}

/** 运行时设置。 */
export interface Settings {
  full_body: boolean;
  max_request_bytes: number;
  /** 上游非流式响应体上限（字节）；与入站上限独立。 */
  max_response_bytes: number;
  /** 请求日志 body 截断上限（字节）；与入站上限独立。 */
  log_body_max_bytes: number;
  /** 价格目录自动同步间隔（天）；`0` 表示只手动同步。 */
  catalog_sync_interval_days: number;
  /** 同一 IP 窗口内允许的认证失败次数；`0` 表示关闭限流。 */
  auth_throttle_max_failures: number;
  /** 认证失败计数窗口（秒）。 */
  auth_throttle_window_secs: number;
  /** SSE 重装缓冲上限（字节）。 */
  sse_reassembly_max_bytes: number;
  /** 同渠道重试基础间隔（毫秒）。 */
  retry_backoff_ms: number;
  /** 同渠道指数退避封顶（毫秒）。 */
  retry_backoff_cap_ms: number;
  /** 上游 Retry-After 最大等待（秒）。 */
  retry_after_cap_secs: number;
  /** 未单独配置限速的令牌使用的每分钟请求兜底；`0` 表示不设全局上限。 */
  rate_limit_rpm: number;
}

/** 目录中一条提供方 × 模型的四档单价（micro-USD / 1M tokens）。 */
export interface CatalogModel {
  provider_id: string;
  provider_name: string;
  model_id: string;
  input_micros: number | null;
  output_micros: number | null;
  cache_read_micros: number | null;
  cache_write_micros: number | null;
}

/** 价格目录读视图：缓存行 + 上次成功同步时刻。 */
export interface CatalogView {
  /** 上次成功写入缓存的 unix 毫秒；从未同步为 null。 */
  synced_at: number | null;
  models: CatalogModel[];
}

/** 目录提供方摘要。 */
export interface CatalogProvider {
  id: string;
  name: string;
  count: number;
}

/** 目录元数据：同步时刻 + 提供方列表。 */
export interface CatalogMeta {
  synced_at: number | null;
  providers: CatalogProvider[];
}

/** `GET /catalog` 过滤参数；都缺省时返回全表。 */
export interface CatalogQuery {
  q?: string;
  /** 单个提供方 id，或多个 id（客户端会打成逗号分隔）。 */
  provider_id?: string | string[];
}

/** 余额相对调整请求。 */
export interface BalanceAdjustment {
  delta_usd_micros: number;
}

/** 余额视图。 */
export interface BalanceView {
  token_key: string;
  balance_usd_micros: number;
  settled_usd_micros: number;
}

/** 请求日志条目；完整 body 为 base64。 */
export interface LogEntry {
  id: number;
  created_at: number;
  token_name: string;
  token_key: string;
  inbound_protocol: string;
  model: string;
  /** 实际出站模型名；旧行可能为 null。 */
  outbound_model: string | null;
  channel: string;
  status_code: number;
  latency_ms: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  input_price_usd_micros: number;
  output_price_usd_micros: number;
  cache_read_price_usd_micros: number;
  cache_write_price_usd_micros: number;
  cost_usd_micros: number;
  /** 费用是否已写入 token_balance。 */
  settled: boolean;
  /** 列表接口为 null；详情 `GET /logs/{id}` 才返回 base64 body。 */
  request_body: string | null;
  /** 列表接口为 null；详情 `GET /logs/{id}` 才返回 base64 body。 */
  response_body: string | null;
}

/** 分页信封。 */
export interface Page<T> {
  items: T[];
  page: number;
  page_size: number;
  total: number;
}

/** 请求日志分页：额外带未结算条数，便于对账。 */
export interface LogPage extends Page<LogEntry> {
  unsettled_total: number;
}

/** 日志列表查询。 */
export interface LogQuery {
  token_key?: string;
  model?: string;
  /** 综合关键字：对令牌/模型/渠道做子串匹配（OR）。 */
  keyword?: string;
  from_created_at?: number;
  to_created_at?: number;
  settled?: boolean;
  page?: number;
  page_size?: number;
}

/** 系统日志条目。 */
export interface SystemLogEntry {
  id: number;
  created_at: number;
  level: string;
  target: string;
  message: string;
}

/** 系统日志查询。`level` / `target` 为分面多选，请求时拼成逗号列表。 */
export interface SystemLogQuery {
  keyword?: string;
  from_created_at?: number;
  to_created_at?: number;
  level?: string[];
  target?: string[];
  page?: number;
  page_size?: number;
}

/** 系统日志分页：额外带当前过滤下出现过的 target，供分面筛选。 */
export interface SystemLogPage extends Page<SystemLogEntry> {
  targets: string[];
}

/** `/stats` 汇总卡片。 */
export interface StatsSummary {
  request_count: number;
  success_count: number;
  input_tokens: number;
  output_tokens: number;
  cost_usd_micros: number;
  token_count: number;
  channel_count: number;
}

/** `/stats` 趋势点。`date` 在 `days=1` 时为 UTC 小时 `YYYY-MM-DDTHH:00:00Z`，否则为日历日 `YYYY-MM-DD`。 */
export interface DailyPoint {
  date: string;
  request_count: number;
  input_tokens: number;
  output_tokens: number;
  cost_usd_micros: number;
}

/** 按模型分布。 */
export interface ModelShare {
  model: string;
  request_count: number;
  cost_usd_micros: number;
}

/** 按渠道分布。 */
export interface ChannelShare {
  channel: string;
  request_count: number;
  cost_usd_micros: number;
}

/** `/stats` 响应。 */
export interface StatsView {
  summary: StatsSummary;
  daily: DailyPoint[];
  by_model: ModelShare[];
  by_channel: ChannelShare[];
}

/** `/stats/lifetime` 全量累计，不受时间窗影响。
 *
 * `request_count` 与 `total_tokens` 含未结算行；`cost_usd_micros` 只计已结算的成功费用。
 */
export interface LifetimeStats {
  request_count: number;
  cost_usd_micros: number;
  total_tokens: number;
}

/** 渠道连通性探测结果。 */
export interface ChannelProbeResult {
  reachable: boolean;
  timed_out: boolean;
  status_code: number | null;
  latency_ms: number;
  error: string | null;
  upstream_body: string | null;
}

/** 拉取上游模型列表的草稿请求：渠道无需已保存。 */
export interface UpstreamModelsDraft {
  protocol: Protocol;
  base_url: string;
  api_key: string;
  timeout_ms: number;
}

/** 上游模型列表响应：模型 id 数组（上游顺序）。 */
export interface UpstreamModelsView {
  models: string[];
}

/** 管理 API 结构化错误体。 */
export interface ApiErrorBody {
  error?: {
    code?: string;
    message?: string;
  };
}

/** 管理 API 客户端错误。 */
export class ApiClientError extends Error {
  readonly code?: string;

  constructor(message: string, code?: string) {
    super(message);
    this.name = 'ApiClientError';
    if (code !== undefined) {
      this.code = code;
    }
  }
}
