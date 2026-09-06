-- 用户列表统计（list_users_stats / get_user_stats）对 request_log 全量
-- GROUP BY user_id：覆盖索引让聚合走 index-only 扫描（免去逐行回表与
-- GROUP BY 的临时排序），用户页打开成本从「整表扫描 + 排序」降为
-- 「紧凑索引顺序扫描」。对账明细仍以主表为准，统计口径不变。
CREATE INDEX IF NOT EXISTS idx_request_log_user_stats
    ON request_log (user_id, request_id, id, input_tokens, output_tokens);
