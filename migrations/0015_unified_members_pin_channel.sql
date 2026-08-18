-- 统一模型成员从「已登记名」改为「渠道 × 已登记名」。
-- 旧 `models_json` 是字符串数组；同名若挂在多条渠道上展开为多条成员，保序后按渠道 id。

UPDATE unified_models
SET models_json = COALESCE((
    SELECT json_group_array(json_object('channel_id', pinned.channel_id, 'model', pinned.model))
    FROM (
        SELECT c.id AS channel_id, m.value AS model
        FROM json_each(unified_models.models_json) AS m
        JOIN channels AS c
          ON EXISTS (
              SELECT 1 FROM json_each(c.models_json) AS listed
              WHERE listed.value = m.value
          )
          OR EXISTS (
              SELECT 1 FROM json_each(c.model_aliases_json) AS alias
              WHERE alias.key = m.value
          )
        ORDER BY m.id, c.id
    ) AS pinned
), '[]')
WHERE json_array_length(models_json) = 0
   OR json_type(json_extract(models_json, '$[0]')) = 'text';
