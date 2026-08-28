-- 系统日志事件采用稳定编码与结构化参数，message 继续保留作旧数据与未知事件的回退。
ALTER TABLE system_log ADD COLUMN event_code TEXT;
ALTER TABLE system_log ADD COLUMN event_params TEXT CHECK (
    (event_code IS NULL AND event_params IS NULL)
    OR (event_code IS NOT NULL AND event_params IS NOT NULL AND json_valid(event_params))
);
