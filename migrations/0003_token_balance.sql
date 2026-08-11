-- 计费（#04）：令牌余额表 + 请求日志计费列。
-- 金额一律整数 micro-USD（ADR-0002），抑制浮点误差。

-- 令牌动态余额：首次出现时按配置 balance_usd 落库，此后只由结算改变。
-- settled_usd_micros 为累计结算总额，用于 limit_usd 上限检查（与余额相互独立）。
CREATE TABLE IF NOT EXISTS token_balance (
    token_key TEXT PRIMARY KEY,
    balance_usd_micros INTEGER NOT NULL,
    settled_usd_micros INTEGER NOT NULL,
    created_at INTEGER NOT NULL          -- unix 毫秒
);

-- 请求日志计费列：usage 四分量 + 费用 + 计费时的价格快照（调价后历史账单可复核）。
ALTER TABLE request_log
    ADD COLUMN token_key TEXT NOT NULL DEFAULT '';

ALTER TABLE request_log
    ADD COLUMN input_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE request_log
    ADD COLUMN output_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE request_log
    ADD COLUMN cache_read_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE request_log
    ADD COLUMN cache_write_tokens INTEGER NOT NULL DEFAULT 0;

ALTER TABLE request_log
    ADD COLUMN input_price_usd_micros INTEGER NOT NULL DEFAULT 0;
ALTER TABLE request_log
    ADD COLUMN output_price_usd_micros INTEGER NOT NULL DEFAULT 0;
ALTER TABLE request_log
    ADD COLUMN cache_read_price_usd_micros INTEGER NOT NULL DEFAULT 0;
ALTER TABLE request_log
    ADD COLUMN cache_write_price_usd_micros INTEGER NOT NULL DEFAULT 0;

ALTER TABLE request_log
    ADD COLUMN cost_usd_micros INTEGER NOT NULL DEFAULT 0;
