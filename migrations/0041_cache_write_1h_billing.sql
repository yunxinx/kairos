-- cache 写入 1h 分档计费（可选策略）：价格行配置 1h 费率才启用分档，
-- 缺省（NULL）整行按 cache_write 单一费率计，存量行为不变。请求日志同步
-- 记录 1h 写入明细与 1h 价格快照，保持每档「token + 价格」的对账完备性。

ALTER TABLE prices ADD COLUMN cache_write_1h_micros INTEGER;

ALTER TABLE request_log ADD COLUMN cache_write_1h_tokens INTEGER NOT NULL DEFAULT 0;

ALTER TABLE request_log ADD COLUMN cache_write_1h_price_usd_micros INTEGER NOT NULL DEFAULT 0;
