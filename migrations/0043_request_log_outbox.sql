-- 请求完成后先持久化待结算记录，再由后台写入最终请求日志。
-- 费用列单独保存，供准入在后台处理前计入余额与累计上限；正文保持原始 BLOB，
-- 避免把大请求体编码进元数据。

CREATE TABLE request_log_outbox (
    id INTEGER PRIMARY KEY,
    token_key TEXT NOT NULL,
    user_id INTEGER NOT NULL,
    cost_usd_micros INTEGER NOT NULL CHECK (cost_usd_micros >= 0),
    metadata BLOB NOT NULL,
    request_body BLOB,
    response_body BLOB
) STRICT;

CREATE INDEX idx_request_log_outbox_token_key
    ON request_log_outbox (token_key);

CREATE INDEX idx_request_log_outbox_user_id
    ON request_log_outbox (user_id);
