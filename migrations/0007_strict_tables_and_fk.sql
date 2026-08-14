-- 硬化存量表（SQLite 坏默认值治理）：
-- 1) 全部业务表重建为 STRICT——SQLite 默认只按「类型亲和性」转换而非校验，
--    INTEGER 列可被写入任意 TEXT；STRICT 让错类型直接报错。
-- 2) token_balance 增加对 tokens 的外键：余额行不再可能脱离令牌定义残留
--    （删除令牌残留余额 → 同 key 重建复活旧余额的隐患由库层面兜底）。
--    request_log.token_key 刻意不加外键：日志须在令牌删除后保留作对账历史。
--
-- 重建采用 SQLite 官方 12 步流程的等价做法：建新表 → 复制 → 改名腾位 →
-- 改名就位。整段在 sqlx 迁移事务内执行；孤儿余额行在建外键前清理，即时
-- 外键逐语句检查，各语句均可通过。
-- request_body/response_body 复制时 CAST 为 BLOB：旧库 BLOB 列按亲和性可能
-- 被写入 TEXT，直接复制进 STRICT 表会报类型错误；CAST 字节无损。
-- AUTOINCREMENT 计数器：改名会连同迁移 sqlite_sequence 条目，先建临时表
-- 复制、再改名就位即可带过现存最大 id（注意：若历史最大 id 行已被删除，
-- 序列只保留到现存最大 id，这是重建流程固有的边界）。

-- --- smoke_probe ---
CREATE TABLE smoke_probe_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    note TEXT NOT NULL
) STRICT;
INSERT INTO smoke_probe_new (id, note) SELECT id, note FROM smoke_probe;
DROP TABLE smoke_probe;
ALTER TABLE smoke_probe_new RENAME TO smoke_probe;

-- --- tokens（先于 token_balance：外键引用方须已就位）---
CREATE TABLE tokens_new (
    token_key TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    limit_usd_micros INTEGER,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL DEFAULT 0,
    last_used_at INTEGER
) STRICT;
INSERT INTO tokens_new (token_key, name, limit_usd_micros, enabled, created_at, last_used_at)
    SELECT token_key, name, limit_usd_micros, enabled, created_at, last_used_at FROM tokens;
DROP TABLE tokens;
ALTER TABLE tokens_new RENAME TO tokens;

-- --- token_balance ---
-- 清理孤儿余额行（如有）：此时新 tokens 表已就位，随后建的外键要求余额行
-- 都能对应现存令牌；孤儿行没有归属令牌，余额也无从操作，直接丢弃。
DELETE FROM token_balance
    WHERE token_key NOT IN (SELECT token_key FROM tokens);
CREATE TABLE token_balance_new (
    token_key TEXT PRIMARY KEY REFERENCES tokens(token_key) ON DELETE CASCADE,
    balance_usd_micros INTEGER NOT NULL,
    settled_usd_micros INTEGER NOT NULL,
    created_at INTEGER NOT NULL          -- unix 毫秒
) STRICT;
INSERT INTO token_balance_new (token_key, balance_usd_micros, settled_usd_micros, created_at)
    SELECT token_key, balance_usd_micros, settled_usd_micros, created_at FROM token_balance;
DROP TABLE token_balance;
ALTER TABLE token_balance_new RENAME TO token_balance;

-- --- request_log ---
CREATE TABLE request_log_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL,          -- unix 毫秒
    token_name TEXT NOT NULL,
    inbound_protocol TEXT NOT NULL,
    model TEXT NOT NULL,
    channel TEXT NOT NULL,
    status_code INTEGER NOT NULL,
    latency_ms INTEGER NOT NULL,
    token_key TEXT NOT NULL DEFAULT '',
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    input_price_usd_micros INTEGER NOT NULL DEFAULT 0,
    output_price_usd_micros INTEGER NOT NULL DEFAULT 0,
    cache_read_price_usd_micros INTEGER NOT NULL DEFAULT 0,
    cache_write_price_usd_micros INTEGER NOT NULL DEFAULT 0,
    cost_usd_micros INTEGER NOT NULL DEFAULT 0,
    request_body BLOB,
    response_body BLOB
) STRICT;
INSERT INTO request_log_new (
    id, created_at, token_name, inbound_protocol, model, channel, status_code,
    latency_ms, token_key, input_tokens, output_tokens, cache_read_tokens,
    cache_write_tokens, input_price_usd_micros, output_price_usd_micros,
    cache_read_price_usd_micros, cache_write_price_usd_micros, cost_usd_micros,
    request_body, response_body)
SELECT id, created_at, token_name, inbound_protocol, model, channel, status_code,
       latency_ms, token_key, input_tokens, output_tokens, cache_read_tokens,
       cache_write_tokens, input_price_usd_micros, output_price_usd_micros,
       cache_read_price_usd_micros, cache_write_price_usd_micros, cost_usd_micros,
       CAST(request_body AS BLOB), CAST(response_body AS BLOB)
FROM request_log;
DROP TABLE request_log;
ALTER TABLE request_log_new RENAME TO request_log;

-- --- channels ---
CREATE TABLE channels_new (
    name TEXT PRIMARY KEY,
    protocol TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key TEXT NOT NULL,
    models_json TEXT NOT NULL,
    model_aliases_json TEXT NOT NULL,
    priority INTEGER NOT NULL,
    weight INTEGER NOT NULL,
    timeout_ms INTEGER NOT NULL,
    max_retries INTEGER NOT NULL
) STRICT;
INSERT INTO channels_new (name, protocol, base_url, api_key, models_json,
    model_aliases_json, priority, weight, timeout_ms, max_retries)
SELECT name, protocol, base_url, api_key, models_json,
       model_aliases_json, priority, weight, timeout_ms, max_retries
FROM channels;
DROP TABLE channels;
ALTER TABLE channels_new RENAME TO channels;

-- --- prices ---
CREATE TABLE prices_new (
    model TEXT PRIMARY KEY,
    input_micros INTEGER NOT NULL,
    output_micros INTEGER NOT NULL,
    cache_read_micros INTEGER,
    cache_write_micros INTEGER
) STRICT;
INSERT INTO prices_new (model, input_micros, output_micros, cache_read_micros, cache_write_micros)
    SELECT model, input_micros, output_micros, cache_read_micros, cache_write_micros FROM prices;
DROP TABLE prices;
ALTER TABLE prices_new RENAME TO prices;

-- --- settings ---
CREATE TABLE settings_new (
    setting_key TEXT PRIMARY KEY,
    setting_value TEXT NOT NULL
) STRICT;
INSERT INTO settings_new (setting_key, setting_value)
    SELECT setting_key, setting_value FROM settings;
DROP TABLE settings;
ALTER TABLE settings_new RENAME TO settings;
