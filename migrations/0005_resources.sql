-- v2 运行时资源入库（#01）：渠道、令牌、价格、设置四类资源移入 SQLite。
-- 金额一律整数 micro-USD（ADR-0002）；价格缓存档 NULL 表示回退 input 价。

-- 渠道：指向一个上游端点的出站接入单元。
-- models / model_aliases 为网关侧自有集合结构，以 JSON 文本存储，不做关系化。
CREATE TABLE IF NOT EXISTS channels (
    name TEXT PRIMARY KEY,
    protocol TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key TEXT NOT NULL,
    models_json TEXT NOT NULL,
    model_aliases_json TEXT NOT NULL,
    priority INTEGER NOT NULL,
    weight INTEGER NOT NULL,
    timeout_ms INTEGER NOT NULL,
    max_retries INTEGER NOT NULL
);

-- 令牌：认证与计费的最小单位；余额独立存 token_balance 表（#0003），
-- 修改令牌属性不重置余额。
-- limit_usd_micros 为累计结算上限，NULL 表示无上限。
CREATE TABLE IF NOT EXISTS tokens (
    token_key TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    limit_usd_micros INTEGER
);

-- 价格：每模型一行，四档 micro-USD / 1M tokens 单价。
CREATE TABLE IF NOT EXISTS prices (
    model TEXT PRIMARY KEY,
    input_micros INTEGER NOT NULL,
    output_micros INTEGER NOT NULL,
    cache_read_micros INTEGER,
    cache_write_micros INTEGER
);

-- 运行时开关：键值表，值为 JSON 编码（bool / 整数 / 字符串）。
CREATE TABLE IF NOT EXISTS settings (
    setting_key TEXT PRIMARY KEY,
    setting_value TEXT NOT NULL
);
