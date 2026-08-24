-- 请求日志仅记录密钥身份，不保存密钥明文。
ALTER TABLE request_log ADD COLUMN channel_key TEXT;
