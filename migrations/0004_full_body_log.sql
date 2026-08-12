-- 可选的完整请求/响应 body：默认 NULL，避免隐私与存储开销。
ALTER TABLE request_log ADD COLUMN request_body BLOB;
ALTER TABLE request_log ADD COLUMN response_body BLOB;
