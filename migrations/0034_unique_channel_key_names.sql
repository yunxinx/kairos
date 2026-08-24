-- 日志以渠道名 + 密钥名标识实际使用的凭据；渠道内名称必须稳定唯一。
-- 旧版本允许同名，保留最早一把的名称，其余按稳定 id 改名后再加约束。
UPDATE channel_keys
SET name = name || ' (legacy ' || id || ')'
WHERE id IN (
    SELECT later.id
    FROM channel_keys AS later
    WHERE EXISTS (
        SELECT 1
        FROM channel_keys AS earlier
        WHERE earlier.channel_id = later.channel_id
          AND earlier.name = later.name
          AND earlier.id < later.id
    )
);

CREATE UNIQUE INDEX channel_keys_channel_id_name_unique
ON channel_keys(channel_id, name);
