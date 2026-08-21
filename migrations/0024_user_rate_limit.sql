-- 用户级每分钟请求上限（RPM）
ALTER TABLE users ADD COLUMN rate_limit_rpm INTEGER;
