-- 冒烟测试用表：验证 axum SSE → reqwest 流式 → sqlx 落库全链路。
CREATE TABLE IF NOT EXISTS smoke_probe (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    note TEXT NOT NULL
);
