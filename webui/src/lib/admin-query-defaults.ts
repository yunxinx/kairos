/** 管理页查询默认值：页面初始状态与导航预取共用，集中一处避免两处漂移。 */

export const OVERVIEW_DEFAULT_DAYS = 7;

export const LOGS_INITIAL_PAGE = 1;
export const LOGS_INITIAL_PAGE_SIZE = 20;
/** 与 LogsFeature 请求日志页的初始 queryKey 形状一致（无筛选）。 */
export const LOGS_INITIAL_QUERY_KEY = [
  'logs',
  LOGS_INITIAL_PAGE,
  LOGS_INITIAL_PAGE_SIZE,
  '',
  null,
  null,
  [],
];
