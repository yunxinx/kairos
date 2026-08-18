-- 下游一次入站请求的稳定身份：统一模型 hop / 同渠道重试会落多条对账行，
-- 同一 request_id 供 /stats 去重为「下游请求数」。存量行保持 NULL，聚合时回退到 id。
ALTER TABLE request_log ADD COLUMN request_id TEXT;
CREATE INDEX IF NOT EXISTS idx_request_log_request_id ON request_log (request_id);
