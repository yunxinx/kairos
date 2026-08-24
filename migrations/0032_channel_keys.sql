-- 渠道密钥从渠道定义中拆出。渠道、价格、顺序表彼此都有外键，不能直接
-- DROP 父表：SQLite 开启外键时会把 ON DELETE CASCADE 子表一并清空。
-- 先建立所有新表并复制数据，再删旧子表和旧父表，最后一次性换名。

CREATE TABLE channel_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    api_key TEXT NOT NULL,
    weight INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1,
    models_json TEXT,
    blocked_models_json TEXT,
    created_at INTEGER NOT NULL
) STRICT;

INSERT INTO channel_keys (channel_id, name, api_key, created_at)
SELECT id, 'default', api_key, CAST(strftime('%s','now') AS INTEGER) * 1000
FROM channels
WHERE api_key <> '';

CREATE TABLE channels_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    protocol TEXT NOT NULL,
    base_url TEXT NOT NULL,
    models_json TEXT NOT NULL,
    model_aliases_json TEXT NOT NULL,
    timeout_ms INTEGER NOT NULL,
    max_retries INTEGER NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    model_group TEXT NOT NULL DEFAULT 'default' REFERENCES model_groups(name)
) STRICT;
INSERT INTO channels_new (id, name, protocol, base_url, models_json, model_aliases_json,
    timeout_ms, max_retries, enabled, model_group)
SELECT id, name, protocol, base_url, models_json, model_aliases_json,
    timeout_ms, max_retries, enabled, model_group FROM channels;

CREATE TABLE channel_keys_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id INTEGER NOT NULL REFERENCES channels_new(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    api_key TEXT NOT NULL,
    weight INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1,
    models_json TEXT,
    blocked_models_json TEXT,
    created_at INTEGER NOT NULL
) STRICT;
INSERT INTO channel_keys_new (
    id, channel_id, name, api_key, weight, enabled, models_json, blocked_models_json, created_at
)
SELECT id, channel_id, name, api_key, weight, enabled, models_json, blocked_models_json, created_at
FROM channel_keys;

CREATE TABLE prices_new (
    channel_id INTEGER NOT NULL,
    model TEXT NOT NULL,
    input_micros INTEGER NOT NULL,
    output_micros INTEGER NOT NULL,
    cache_read_micros INTEGER,
    cache_write_micros INTEGER,
    PRIMARY KEY (channel_id, model),
    FOREIGN KEY (channel_id) REFERENCES channels_new(id) ON DELETE CASCADE
) STRICT;
INSERT INTO prices_new (
    channel_id, model, input_micros, output_micros, cache_read_micros, cache_write_micros
)
SELECT channel_id, model, input_micros, output_micros, cache_read_micros, cache_write_micros
FROM prices;

CREATE TABLE channel_model_order_new (
    model TEXT NOT NULL,
    channel_id INTEGER NOT NULL REFERENCES channels_new(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (model, channel_id)
) STRICT;
INSERT INTO channel_model_order_new (model, channel_id, position)
SELECT model, channel_id, position FROM channel_model_order;

DROP TABLE channel_keys;
DROP TABLE prices;
DROP TABLE channel_model_order;
DROP TABLE channels;

ALTER TABLE channels_new RENAME TO channels;
ALTER TABLE channel_keys_new RENAME TO channel_keys;
ALTER TABLE prices_new RENAME TO prices;
ALTER TABLE channel_model_order_new RENAME TO channel_model_order;
