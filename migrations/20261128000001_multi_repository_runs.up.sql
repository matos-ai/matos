-- A change run may write to repositories besides its own: the same branch is
-- pushed to each, and each changed repository gets its own pull request.
ALTER TABLE runs ADD COLUMN extra_repositories TEXT[] NOT NULL DEFAULT '{}';
ALTER TABLE task_grants ADD COLUMN writable_repositories TEXT[] NOT NULL DEFAULT '{}';
