-- 折扣率结算：请求日志补记渠道原价与折扣率。
--
-- cost_usd_micros 继续表示实收（折后）；base_cost_usd_micros 为渠道原价，
-- discount_bp 为该次使用的万分比折扣率。存量行原本只有原价即实收，
-- 因此回填 base_cost_usd_micros = cost_usd_micros、discount_bp = 10000。

ALTER TABLE request_log ADD COLUMN base_cost_usd_micros INTEGER NOT NULL DEFAULT 0;
ALTER TABLE request_log ADD COLUMN discount_bp INTEGER NOT NULL DEFAULT 10000;

UPDATE request_log SET base_cost_usd_micros = cost_usd_micros;
UPDATE request_log SET discount_bp = 10000;
