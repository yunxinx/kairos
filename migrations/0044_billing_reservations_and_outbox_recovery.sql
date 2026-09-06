-- 出站尝试级预留与结算队列恢复状态。
--
-- attempt_id 唯一标识一次实际出站尝试；request_id 只聚合同一次入站请求产生的
-- 多次重试、渠道切换和统一模型跳转。二者分离后，每次已经发出的尝试都能独立
-- 保存结果并且只结算一次，不会由后续尝试覆盖前序账务状态。
CREATE TABLE billing_reservations (
    attempt_id TEXT PRIMARY KEY,
    request_id TEXT NOT NULL,
    token_key TEXT NOT NULL,
    user_id INTEGER NOT NULL,
    reserved_cost_usd_micros INTEGER NOT NULL CHECK (reserved_cost_usd_micros >= 0),
    token_limit_usd_micros INTEGER,
    recovery_metadata BLOB NOT NULL,
    actual_cost_usd_micros INTEGER,
    status TEXT NOT NULL CHECK (status IN ('reserved', 'settled', 'released')),
    dispatched INTEGER NOT NULL DEFAULT 0 CHECK (dispatched IN (0, 1)),
    result_persisted INTEGER NOT NULL DEFAULT 0 CHECK (result_persisted IN (0, 1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_billing_reservations_request
    ON billing_reservations (request_id, created_at);
CREATE INDEX idx_billing_reservations_user_pending
    ON billing_reservations (user_id, status);
CREATE INDEX idx_billing_reservations_token_pending
    ON billing_reservations (token_key, status);

ALTER TABLE request_log_outbox ADD COLUMN request_id TEXT;
ALTER TABLE request_log_outbox ADD COLUMN billing_attempt_id TEXT;
ALTER TABLE request_log_outbox ADD COLUMN state TEXT NOT NULL DEFAULT 'queued'
    CHECK (state IN ('queued', 'isolated'));
ALTER TABLE request_log_outbox ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE request_log_outbox ADD COLUMN next_retry_at INTEGER;
ALTER TABLE request_log_outbox ADD COLUMN last_error TEXT;

CREATE INDEX idx_request_log_outbox_ready
    ON request_log_outbox (state, next_retry_at, id);
CREATE UNIQUE INDEX idx_request_log_outbox_billing_attempt
    ON request_log_outbox (billing_attempt_id)
    WHERE billing_attempt_id IS NOT NULL;

ALTER TABLE request_log ADD COLUMN billing_attempt_id TEXT;
CREATE UNIQUE INDEX idx_request_log_billing_attempt
    ON request_log (billing_attempt_id)
    WHERE billing_attempt_id IS NOT NULL;

-- 内置管理员档默认拥有模型配置的只读视图；写能力仍保持原有独立开关。
UPDATE plans
SET capabilities_json = json_set(
    capabilities_json,
    '$.view_channels', json('true'),
    '$.view_prices', json('true'),
    '$.view_model_groups', json('true'),
    '$.view_unified_models', json('true')
)
WHERE id = 2;
