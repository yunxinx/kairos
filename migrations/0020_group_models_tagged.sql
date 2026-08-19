-- 模型组名单从「可调用名」改为 tagged 条目（渠道 × 名，或统一 ID）。
-- 旧 `models_json` 是字符串数组。统一 ID 收成 kind=unified；挂在渠道上的名
-- 按渠道展开为 kind=source（同名多渠道多条，保序后先统一再按渠道 id）。
-- 既不是统一 ID、也不在任何渠道清单/别名里的名字丢掉（无法表示成条目）。
-- 无 kind 的 {channel_id, model} 只补 kind=source。已带 kind 的对象不动。

UPDATE model_groups
SET models_json = COALESCE((
    SELECT json_group_array(json(rewritten.entry))
    FROM (
        SELECT m.key AS ord, 0 AS tag, 0 AS channel_id,
               json_object('kind', 'unified', 'id', m.value) AS entry
        FROM json_each(model_groups.models_json) AS m
        WHERE m.type = 'text'
          AND EXISTS (SELECT 1 FROM unified_models AS u WHERE u.id = m.value)

        UNION ALL

        SELECT m.key AS ord, 1 AS tag, c.id AS channel_id,
               json_object('kind', 'source', 'channel_id', c.id, 'model', m.value) AS entry
        FROM json_each(model_groups.models_json) AS m
        JOIN channels AS c
          ON m.type = 'text'
         AND (
            EXISTS (
                SELECT 1 FROM json_each(c.models_json) AS listed
                WHERE listed.value = m.value
            )
            OR EXISTS (
                SELECT 1 FROM json_each(c.model_aliases_json) AS alias
                WHERE alias.key = m.value
            )
         )

        ORDER BY 1, 2, 3
    ) AS rewritten
), '[]')
WHERE json_array_length(models_json) = 0
   OR json_type(models_json, '$[0]') = 'text';

UPDATE model_groups
SET models_json = COALESCE((
    SELECT json_group_array(
        json_object(
            'kind', 'source',
            'channel_id', json_extract(m.value, '$.channel_id'),
            'model', json_extract(m.value, '$.model')
        )
    )
    FROM json_each(model_groups.models_json) AS m
    ORDER BY m.key
), '[]')
WHERE json_type(models_json, '$[0]') = 'object'
  AND json_type(models_json, '$[0].kind') IS NULL;
