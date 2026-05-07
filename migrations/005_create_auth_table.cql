CREATE TABLE IF NOT EXISTS equicloud.auth (
    user_id TEXT PRIMARY KEY,
    secret TEXT,
    created_at BIGINT,
    updated_at BIGINT
);
