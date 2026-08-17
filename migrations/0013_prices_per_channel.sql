-- 价格按渠道独立：同一可调用名在不同渠道上各自一行（ADR-0007）。
-- 存量全局价簿展开到当时清单含该名（models 或别名 key）的每条渠道；
-- 没有任何渠道挂过的价格行丢弃（本来也无法计费）。
-- 渠道删除时 CASCADE 清掉该渠道价格。

CREATE TABLE prices_new (
    channel_id INTEGER NOT NULL,
    model TEXT NOT NULL,
    input_micros INTEGER NOT NULL,
    output_micros INTEGER NOT NULL,
    cache_read_micros INTEGER,
    cache_write_micros INTEGER,
    PRIMARY KEY (channel_id, model),
    FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE
) STRICT;

INSERT INTO prices_new (
    channel_id, model, input_micros, output_micros, cache_read_micros, cache_write_micros
)
SELECT
    c.id,
    p.model,
    p.input_micros,
    p.output_micros,
    p.cache_read_micros,
    p.cache_write_micros
FROM prices AS p
JOIN channels AS c
    ON EXISTS (SELECT 1 FROM json_each(c.models_json) AS listed WHERE listed.value = p.model)
    OR EXISTS (
        SELECT 1 FROM json_each(c.model_aliases_json) AS alias WHERE alias.key = p.model
    );

DROP TABLE prices;
ALTER TABLE prices_new RENAME TO prices;
