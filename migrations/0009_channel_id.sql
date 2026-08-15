-- 渠道身份改为库生成 id：name 从主键降为 UNIQUE 约束，改名不再需要删行重建。
-- id 用 AUTOINCREMENT：id 是管理 API 与 UI 的稳定身份，不允许回收复用。
-- 重建沿用 0007 的「建新表 → 复制 → 改名」流程；存量行无 id，复制时由
-- AUTOINCREMENT 按行序生成。

CREATE TABLE channels_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    protocol TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key TEXT NOT NULL,
    models_json TEXT NOT NULL,
    model_aliases_json TEXT NOT NULL,
    priority INTEGER NOT NULL,
    weight INTEGER NOT NULL,
    timeout_ms INTEGER NOT NULL,
    max_retries INTEGER NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1
) STRICT;
INSERT INTO channels_new (name, protocol, base_url, api_key, models_json,
    model_aliases_json, priority, weight, timeout_ms, max_retries, enabled)
SELECT name, protocol, base_url, api_key, models_json,
       model_aliases_json, priority, weight, timeout_ms, max_retries, enabled
FROM channels;
DROP TABLE channels;
ALTER TABLE channels_new RENAME TO channels;
