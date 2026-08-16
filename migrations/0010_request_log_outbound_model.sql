-- 请求日志增加实际出站模型名：别名改写后（或日后统一模型落到的已登记模型）。
-- 存量行保持 NULL（可视为等于入站 `model`）；新行写入出站名。
ALTER TABLE request_log ADD COLUMN outbound_model TEXT;
