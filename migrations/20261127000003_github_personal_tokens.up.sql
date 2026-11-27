-- A member's own fine-grained personal access token, used instead of the App's
-- user authorization while active. Sealed with SECRETS_KEY, AD
-- `github-pat/<org>/<subject>`.
CREATE TABLE github_personal_tokens (
    organization_id UUID NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    subject TEXT NOT NULL,
    github_user_id BIGINT NOT NULL,
    github_login TEXT NOT NULL,
    token_nonce BYTEA NOT NULL,
    token_ciphertext BYTEA NOT NULL,
    token_preview TEXT NOT NULL,
    expires_at TIMESTAMPTZ,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, subject)
);
