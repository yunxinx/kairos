-- 同名可调用名的渠道尝试顺序独立于渠道定义。没有行时由 channels.id 推导；
-- 留存只剩一条渠道的行，便于同名渠道重新出现时恢复运营指定顺序。
--
-- channels 仍被 prices 外键引用。重建时先复制子表，再删旧子表和旧父表，避免
-- ON DELETE CASCADE 在 DROP TABLE 时误删已有价格。

CREATE TABLE channels_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    protocol TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key TEXT NOT NULL,
    models_json TEXT NOT NULL,
    model_aliases_json TEXT NOT NULL,
    timeout_ms INTEGER NOT NULL,
    max_retries INTEGER NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    model_group TEXT NOT NULL DEFAULT 'default' REFERENCES model_groups(name)
) STRICT;

INSERT INTO channels_new (
    id, name, protocol, base_url, api_key, models_json, model_aliases_json,
    timeout_ms, max_retries, enabled, model_group
)
SELECT
    id, name, protocol, base_url, api_key, models_json, model_aliases_json,
    timeout_ms, max_retries, enabled, model_group
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
SELECT
    channel_id, model, input_micros, output_micros, cache_read_micros, cache_write_micros
FROM prices;

DROP TABLE prices;
DROP TABLE channels;
ALTER TABLE channels_new RENAME TO channels;
ALTER TABLE prices_new RENAME TO prices;

CREATE TABLE channel_model_order (
    model TEXT NOT NULL,
    channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (model, channel_id)
) STRICT;
