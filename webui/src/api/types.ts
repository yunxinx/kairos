/** 渠道 wire 协议，与后端 `Protocol` serde rename 一致。 */
export type Protocol = 'openai_chat' | 'openai_responses' | 'anthropic_messages';

/** 令牌写契约；余额与生命周期元数据不在其中。 */
export interface Token {
  token_key: string;
  name: string;
  limit_usd_micros: number | null;
  enabled: boolean;
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

/** 渠道：出站接入单元。 */
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
  status_code: number | null;
  latency_ms: number;
  error: string | null;
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
