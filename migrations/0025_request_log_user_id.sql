-- 请求日志携带归属用户：按归属过滤日志与用量统计需要用户身份。
--
-- `token_key` 不是外键、且令牌可被删除，靠 JOIN tokens 取归属会在删令牌后丢失历史
-- （用量统计因此会凭空缩水）。故把 user_id 冗余进日志行，写入时定格，之后不随
-- 令牌增删变化。`0` 表示存量行或归属未知，查询侧不会匹配任何真实用户。

ALTER TABLE request_log ADD COLUMN user_id INTEGER NOT NULL DEFAULT 0;

UPDATE request_log
SET user_id = COALESCE(
    (SELECT t.user_id FROM tokens t WHERE t.token_key = request_log.token_key),
    0
);

CREATE INDEX idx_request_log_user_id ON request_log (user_id);
