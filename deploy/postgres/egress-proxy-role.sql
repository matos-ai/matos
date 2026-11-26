DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'matos_egress_proxy') THEN
        CREATE ROLE matos_egress_proxy LOGIN;
    END IF;
END
$$;

GRANT USAGE ON SCHEMA public TO matos_egress_proxy;

REVOKE ALL ON ALL TABLES IN SCHEMA public FROM matos_egress_proxy;

GRANT SELECT ON task_grants, provider_credentials, user_provider_credentials, github_installations,
    github_apps
    TO matos_egress_proxy;
