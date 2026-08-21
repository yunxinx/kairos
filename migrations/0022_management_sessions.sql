-- 管理会话：只存会话令牌的 SHA-256，不存明文；吊销后请求即失败。

CREATE TABLE management_sessions (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE INDEX idx_management_sessions_user_id ON management_sessions(user_id);
