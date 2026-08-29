/** 渠道 wire 协议，与后端 `Protocol` serde rename 一致。 */
export type Protocol = 'openai_chat' | 'openai_responses' | 'anthropic_messages';

export const PROTOCOLS: readonly Protocol[] = [
  'openai_chat',
  'openai_responses',
  'anthropic_messages',
];

/** 渠道 reasoning 思维链兼容输出模式，与后端 `ReasoningOutputMode` serde rename 一致。 */
export type ReasoningOutputMode = 'auto' | 'always' | 'off';

export const REASONING_OUTPUT_MODES: readonly ReasoningOutputMode[] = [
  'auto',
  'always',
  'off',
];

/** 渠道会话缓存键回写模式，与后端 `SessionCacheKeyMode` serde rename 一致。 */
export type SessionCacheKeyMode = 'off' | 'auto' | 'always';

export const SESSION_CACHE_KEY_MODES: readonly SessionCacheKeyMode[] = [
  'off',
  'auto',
  'always',
];

/** 运行时收窄日志/表单里的协议字符串。 */
export function isProtocol(value: string): value is Protocol {
  return (PROTOCOLS as readonly string[]).includes(value);
}

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

/** 令牌属性字段；密钥、余额与生命周期元数据不在其中。 */
interface TokenAttributes {
  name: string;
  /** 每分钟请求上限；`null` 跟随全局兜底，`0` 表示该令牌不限速。 */
  rate_limit_rpm: number | null;
  enabled: boolean;
  /** 绑定的模型组名；未指定时为内置 `default`。 */
  model_group: string;
}

/** 令牌属性与可选余额命令构成一个原子更新。 */
export interface TokenUpdate extends TokenAttributes {
  /** 与属性更新同一事务提交的可选余额命令。 */
  balance_change?: TokenBalanceCommand;
}

/** 令牌创建契约：不接受指定 key，key 由系统生成并随响应返回。 */
export interface TokenCreate extends TokenAttributes {
  /** 初始可用余额；`null` 表示无限额。 */
  balance_usd_micros: number | null;
}

export interface BulkDeleteResult<T> {
  deleted: T[];
}

export interface ChannelModelTarget {
  channel_id: number;
  model: string;
}

/** 令牌读响应：属性 + 身份 + 额度与生命周期事实。 */
export interface TokenView extends TokenAttributes {
  /** 库生成的稳定身份；管理面按它定位令牌。 */
  id: number;
  token_key: string;
  /** 累计消费上限；`null` 表示无限额。 */
  limit_usd_micros: number | null;
  /** 派生可用余额 = 累计消费上限 - 累计已结算；`null` 表示无限额。 */
  balance_usd_micros: number | null;
  /** 创建时刻（unix 毫秒）。 */
  created_at: number;
  /** 最后使用时刻（unix 毫秒）；null 表示从未使用。 */
  last_used_at: number | null;
  /** 该令牌累计已结算（micro-USD），对照 `limit_usd_micros`。 */
  settled_usd_micros: number;
}

/** 渠道：出站接入单元（写契约，不含库生成身份）。 */
export interface Channel {
  name: string;
  protocol: Protocol;
  base_url: string;
  /** 该上游端点可用的多把账号密钥。 */
  keys: ChannelKey[];
  models: string[];
  model_aliases: Record<string, string>;
  timeout_ms: number;
  max_retries: number;
  /** 是否启用：禁用的渠道不参与路由与失败切换。 */
  enabled: boolean;
  /** 保存时把新加入的可调用名并入该组；`default` 表示不自动入组。 */
  model_group: string;
  /** reasoning 思维链兼容输出模式；缺省 auto（按厂商提示词表自动判定）。 */
  reasoning_output: ReasoningOutputMode;
  /** 会话缓存键回写模式；缺省 off（不改动出站请求）。 */
  session_cache_key: SessionCacheKeyMode;
}

/** 渠道上的一把上游密钥；模型白/黑名单为可选的逗号名单。 */
export interface ChannelKey {
  name: string;
  api_key: string;
  /** 加权随机权重；全部为 0 时退化为等概率。 */
  weight: number;
  /** 是否启用：禁用的密钥不参与选取。 */
  enabled: boolean;
  /** 模型白名单；`null`/缺省表示不限。 */
  models?: string[] | null;
  /** 模型黑名单；`null`/缺省表示不限。 */
  blocked_models?: string[] | null;
}

/** 渠道读响应：库生成的稳定身份 + 写契约字段。 */
export interface ChannelView extends Channel {
  id: number;
}

/**
 * 渠道名录条目：`GET /channels/summary` 的 admin+ 只读投影。
 *
 * 够回答「某个已登记名挂在哪条渠道、那条渠道还在不在、协议是什么」，不含密钥与
 * 出站地址（仍是 root-only 的运营机密）。`ChannelView` 结构上是它的超集，因此
 * 只读渲染的辅助函数一律以此为参数类型，root 传完整视图也成立。
 */
export interface ChannelSummary {
  id: number;
  name: string;
  protocol: Protocol;
  enabled: boolean;
  models: string[];
  model_aliases: Record<string, string>;
}

/** 读视图 → 写契约：剥离只读 id（后端 `deny_unknown_fields` 拒收未知字段）。 */
export function channelWriteBody(view: ChannelView): Channel {
  return {
    name: view.name,
    protocol: view.protocol,
    base_url: view.base_url,
    keys: view.keys.map((key) => ({ ...key })),
    models: view.models,
    model_aliases: view.model_aliases,
    timeout_ms: view.timeout_ms,
    max_retries: view.max_retries,
    enabled: view.enabled,
    model_group: view.model_group,
    reasoning_output: view.reasoning_output,
    session_cache_key: view.session_cache_key,
  };
}

/** 同名渠道顺序表里的一行：某个可调用名在多条渠道上的完整尝试顺序。 */
export interface ChannelModelOrder {
  model: string;
  channel_ids: number[];
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
  { kind: 'source'; channel_id: number; model: string } | { kind: 'unified'; id: string };

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

/** 人工调整用户钱包的幂等命令。 */
export interface UserBalanceAdjustment {
  operation_id: string;
  delta_usd_micros: number;
  reason: 'manual_adjustment';
}

/** 钱包或令牌余额命令的原始执行结果。 */
export interface BalanceAdjustmentResult {
  operation_id: string;
  before_balance_usd_micros: number | null;
  after_balance_usd_micros: number | null;
}

/** 令牌余额相对调整或显式模式切换命令。 */
export type TokenBalanceCommand =
  | { action: 'adjust'; operation_id: string; delta_usd_micros: number }
  | { action: 'set_finite'; operation_id: string; balance_usd_micros: number }
  | { action: 'set_unlimited'; operation_id: string };

/** 套餐管理面能力开关。 */
export interface PlanCapabilities {
  manage_users: boolean;
  assign_plan: boolean;
  view_logs_stats: boolean;
  settle_waive: boolean;
  toggle_user_tokens: boolean;
  view_own_plan_groups: boolean;
  view_other_groups: boolean;
  edit_prices: boolean;
  edit_model_groups: boolean;
  edit_unified_models: boolean;
  edit_price_catalog: boolean;
}

/**
 * 套餐受众：这一档是给普通用户还是给管理员用的。
 *
 * 决定能力开关是否有意义——用户档不展示它们（后端也不让它们生效）。创建后不可改：
 * 中途切换会让已挂载的用户悄悄获得或失去管理能力。
 */
export type PlanAudience = 'user' | 'admin';

/** 套餐读视图。内部名由系统按 `plan-{id}` 生成，不对外暴露。 */
export interface PlanView {
  id: number;
  display_name: string;
  note: string;
  note_visible_to_admin: boolean;
  discount_bp: number;
  default_rpm: number | null;
  shared_rpm: number | null;
  initial_grant_usd_micros: number;
  capabilities: PlanCapabilities;
  shared_with_admin: boolean;
  audience: PlanAudience;
  /** 是否为本受众新用户的默认档；每个受众至多一档。 */
  is_default: boolean;
  builtin: boolean;
  created_at: number;
  groups: string[];
}

/** 套餐可编辑属性；受众、默认身份与内部名（系统托管）不在更新契约中。 */
export interface PlanUpdate {
  display_name: string;
  note: string;
  note_visible_to_admin: boolean;
  discount_bp: number;
  default_rpm: number | null;
  shared_rpm: number | null;
  initial_grant_usd_micros: number;
  capabilities: PlanCapabilities;
  shared_with_admin: boolean;
  groups: string[];
}

/** 套餐创建契约；受众创建后不可变，默认身份随后只能通过转移命令修改。 */
export interface PlanCreate extends PlanUpdate {
  audience: PlanAudience;
  is_default: boolean;
}

/** 管理角色：上级含下级权限。 */
export type ManagementRole = 'user' | 'admin' | 'root';

const ROLE_RANK: Record<ManagementRole, number> = { user: 0, admin: 1, root: 2 };

/** 是否不低于 `min`（root > admin > user）。 */
export function roleAtLeast(role: ManagementRole, min: ManagementRole): boolean {
  return ROLE_RANK[role] >= ROLE_RANK[min];
}

/** 登录与 `/me` 的身份字段。 */
export interface UserView {
  id: number;
  email: string;
  display_name: string;
  role: ManagementRole;
  enabled: boolean;
  avatar?: string | null;
  rate_limit_rpm?: number | null;
}

/** 当前用户：身份 + 套餐 + 可用组 + 钱包 + 统计。 */
export interface MeView extends UserView {
  plan_id: number | null;
  plan_display_name: string | null;
  discount_bp: number;
  assigned_groups: string[];
  capabilities: PlanCapabilities;
  balance_usd_micros: number;
  settled_usd_micros: number;
  request_count?: number;
  input_tokens?: number;
  output_tokens?: number;
  last_used_at?: number | null;
}

/**
 * 折后单价区间（micro-USD / 1M tokens）。
 *
 * 价格按渠道定，同一个可调用名挂在多条渠道上就可能有多个单价；单渠道时两端相等。
 */
export interface PriceRange {
  min_micros: number;
  max_micros: number;
}

/**
 * `/me/models` 里的一个可调用名。
 *
 * 刻意不含任何渠道字段：这一投影存在的理由就是不向普通用户暴露渠道拓扑。
 */
export interface MyModelView {
  /** 请求 body 的 `model` 直接填它。 */
  id: string;
  /** 统一模型（内部按序 failover），成员渠道不暴露。 */
  unified: boolean;
  /** 当前是否真能调用：有启用且已定价的渠道。 */
  callable: boolean;
  input?: PriceRange;
  output?: PriceRange;
  cache_read?: PriceRange;
  cache_write?: PriceRange;
}

/** 一个模型组一段；同一个名字可以出现在多段里（组是允许名单，不是分区）。 */
export interface MyGroupView {
  name: string;
  models: MyModelView[];
}

/** 调用者自己能用的模型。单价已折过，`discount_bp` 只用于界面标注。 */
export interface MyModelsView {
  discount_bp: number;
  groups: MyGroupView[];
}

/** 用户管理列表/详情。不含 avatar：运营视图不渲染头像，自己的走 `/me`。 */
export interface UserAdminView extends Omit<MeView, 'avatar' | 'capabilities'> {
  request_count: number;
  input_tokens: number;
  output_tokens: number;
  last_used_at: number | null;
}

export interface LoginRequest {
  email: string;
  password: string;
}

export interface LoginView {
  token: string;
  expires_at: number;
  user: UserView;
}

export interface UserCreate {
  email: string;
  display_name: string;
  password: string;
  role: ManagementRole;
  avatar?: string;
  rate_limit_rpm?: number | null;
  plan_id?: number;
}

/** 当前用户改自己的资料。改密码或改邮箱时必须带 `current_password`。 */
export interface MeUpdate {
  email?: string;
  display_name?: string;
  password?: string;
  current_password?: string;
  avatar?: string;
}

export interface UserUpdate {
  /** 修正登录邮箱（建号敲错等场景）；改后目标的其他会话全部吊销。 */
  email?: string;
  role?: ManagementRole;
  enabled?: boolean;
  password?: string;
  display_name?: string;
  avatar?: string;
  /** 与资料字段同一事务中换套餐。 */
  plan_id?: number;
  rate_limit_rpm?: number | null;
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
  /** 渠道原价（折扣前）。 */
  base_cost_usd_micros: number;
  /** 万分比折扣率；10000 表示原价。 */
  discount_bp: number;
  /** 实收（折后）。 */
  cost_usd_micros: number;
  /** 费用是否已完成所属用户钱包结算。 */
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

/** 列表排序方向。 */
export type SortDir = 'asc' | 'desc';

/** 请求日志可排序列：只含有大小关系的量，类别列走筛选。缺省 `created` 倒序。 */
export type RequestLogSortBy = 'created' | 'tokens' | 'latency' | 'cache' | 'cost';

/** 系统日志可排序列：只有时间有顺序。 */
export type SystemLogSortBy = 'created';

/** 日志列表查询。 */
export interface LogQuery {
  token_key?: string;
  /** 按令牌展示名精确过滤；列表里的 `token_key` 已脱敏，行内筛选用这个。 */
  token_name?: string;
  model?: string;
  /** 按渠道名精确过滤。 */
  channel?: string;
  /** 综合关键字：对令牌/模型/渠道做子串匹配（OR）。 */
  keyword?: string;
  from_created_at?: number;
  to_created_at?: number;
  settled?: boolean;
  /** 按该次使用的万分比折扣率精确过滤。 */
  discount_bp?: number;
  /** 入站协议分面多选，请求时拼成逗号列表。 */
  inbound_protocol?: string[];
  sort_by?: RequestLogSortBy;
  sort_dir?: SortDir;
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
  /** 稳定事件编码；旧式自由文本日志为空。 */
  event_code?: string | null;
  /** 事件模板参数；旧式自由文本日志为空。 */
  event_params?: Record<string, unknown> | null;
  /** 操作者；系统自身产生的运维事件为 null。 */
  actor_user_id?: number | null;
  actor_email?: string | null;
}

/** 系统日志查询。`level` / `target` 为分面多选，请求时拼成逗号列表。 */
export interface SystemLogQuery {
  keyword?: string;
  /** 按操作者收窄；缺省不限。 */
  actor_user_id?: number;
  from_created_at?: number;
  to_created_at?: number;
  level?: string[];
  target?: string[];
  sort_by?: SystemLogSortBy;
  sort_dir?: SortDir;
  page?: number;
  page_size?: number;
}

/** 系统日志分页：额外带当前过滤下出现过的 target，供分面筛选。 */
export interface SystemLogPage extends Page<SystemLogEntry> {
  targets: string[];
}

/** 日志占用快照（root 维护视图）。 */
export interface LogSizeView {
  /** 主库文件字节数（含空闲页，删除不回缩、后续写入复用）。 */
  db_size_bytes: number;
  /** WAL 边车字节数；清理收尾的 checkpoint 成功时会截断为零。 */
  wal_size_bytes: number;
  request_log_rows: number;
  system_log_rows: number;
}

/** 按时间窗清理日志的结果。 */
export interface CleanupResultView {
  removed_request_logs: number;
  removed_system_logs: number;
}

/** `/stats` 汇总卡片。 */
export interface StatsSummary {
  request_count: number;
  success_count: number;
  input_tokens: number;
  output_tokens: number;
  cost_usd_micros: number;
  token_count: number;
  /** 出站渠道数；普通用户视图后端整键省略（渠道属运营视角）。 */
  channel_count?: number;
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
