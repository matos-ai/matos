CREATE TABLE run_events (
    run_id UUID NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    seq BIGINT NOT NULL CHECK (seq > 0),
    kind TEXT NOT NULL CHECK (kind IN ('assistant_text', 'tool_use', 'tool_result', 'system')),
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, seq)
);
