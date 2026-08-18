-- 请求日志热表：过滤/聚合常用列加索引；结算失败可标记，避免与余额对账时被当成已扣。
CREATE INDEX IF NOT EXISTS idx_request_log_created_at ON request_log (created_at);
CREATE INDEX IF NOT EXISTS idx_request_log_token_key ON request_log (token_key);
CREATE INDEX IF NOT EXISTS idx_request_log_model ON request_log (model);

-- 1 = 已写入 token_balance；0 = 计费算出了费用但 settle_charge 失败。
ALTER TABLE request_log ADD COLUMN settled INTEGER NOT NULL DEFAULT 1;
