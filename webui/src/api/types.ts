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
  };
}

/** 单模型四档单价（micro-USD / 1M tokens）。 */
export interface Price {
  model: string;
  input_micros: number;
  output_micros: number;
  cache_read_micros: number | null;
  cache_write_micros: number | null;
}

/** 运行时设置。 */
export interface Settings {
  full_body: boolean;
  max_request_bytes: number;
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
  request_body: string | null;
  response_body: string | null;
}

/** 分页信封。 */
export interface Page<T> {
  items: T[];
  page: number;
  page_size: number;
  total: number;
}

/** 日志列表查询。 */
export interface LogQuery {
  token_key?: string;
  model?: string;
  /** 综合关键字：对令牌/模型/渠道做子串匹配（OR）。 */
  keyword?: string;
  from_created_at?: number;
  to_created_at?: number;
  page?: number;
  page_size?: number;
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

/** `/stats/lifetime` 全量累计，不受时间窗影响。 */
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
