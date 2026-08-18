-- 渠道默认模型组：保存时把新加入的可调用名并入该组；`default` 表示不自动入组。
-- 存量渠道一律 default，与「未指定则不自动入组」一致。
--
-- SQLite 在 foreign_keys=ON 时禁止 ALTER TABLE ADD COLUMN 同时带 REFERENCES
-- 与非 NULL 默认值。迁移事务内也不能关外键。因此沿用 0011 的重建：先复制
-- 子表 prices（FK 指向 channels），再 DROP 旧表。

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
    enabled INTEGER NOT NULL DEFAULT 1,
    model_group TEXT NOT NULL DEFAULT 'default' REFERENCES model_groups(name)
) STRICT;

INSERT INTO channels_new (
    id, name, protocol, base_url, api_key, models_json, model_aliases_json,
    priority, weight, timeout_ms, max_retries, enabled, model_group
)
SELECT
    id, name, protocol, base_url, api_key, models_json, model_aliases_json,
    priority, weight, timeout_ms, max_retries, enabled, 'default'
FROM channels;

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

DROP TABLE prices;
DROP TABLE channels;
ALTER TABLE channels_new RENAME TO channels;
ALTER TABLE prices_new RENAME TO prices;

-- models.dev 价格目录缓存：按提供方展开的扁平行，供管理面填价，不进请求快照。
CREATE TABLE catalog_models (
    provider_id TEXT NOT NULL,
    provider_name TEXT NOT NULL,
    model_id TEXT NOT NULL,
    input_micros INTEGER,
    output_micros INTEGER,
    cache_read_micros INTEGER,
    cache_write_micros INTEGER,
    PRIMARY KEY (provider_id, model_id)
) STRICT;
