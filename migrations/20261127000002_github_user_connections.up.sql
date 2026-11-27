-- A member's own GitHub authorization through the organization's GitHub App.
-- Tokens are sealed with SECRETS_KEY, AD `github-user/<org>/<subject>/<access|refresh>`.
CREATE TABLE github_user_connections (
    organization_id UUID NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    subject TEXT NOT NULL,
    github_user_id BIGINT NOT NULL,
    github_login TEXT NOT NULL,
    access_nonce BYTEA NOT NULL,
    access_ciphertext BYTEA NOT NULL,
    access_expires_at TIMESTAMPTZ,
    refresh_nonce BYTEA,
    refresh_ciphertext BYTEA,
    refresh_expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, subject),
    CONSTRAINT github_user_connections_refresh_sealed
        CHECK ((refresh_nonce IS NULL) = (refresh_ciphertext IS NULL))
);

ALTER TABLE task_grants
    ADD COLUMN git_identity TEXT NOT NULL DEFAULT 'app'
        CHECK (git_identity IN ('app', 'member'));
