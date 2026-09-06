-- 渠道会话缓存键回写模式：off（默认，不改动出站请求）、auto（下游未显式
-- 携带 prompt_cache_key 时回写解析出的会话标识）、always（无条件回写覆盖）。
-- 缺省 off，存量渠道出站行为不变。

ALTER TABLE channels ADD COLUMN session_cache_key TEXT NOT NULL DEFAULT 'off';
