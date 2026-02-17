CREATE TABLE IF NOT EXISTS equicloud.data (
    user_id TEXT,
    key TEXT,
    value BLOB,
    version BIGINT,
    checksum TEXT,
    size_bytes INT,
    created_at BIGINT,
    updated_at BIGINT,
    PRIMARY KEY (user_id, key)
);
