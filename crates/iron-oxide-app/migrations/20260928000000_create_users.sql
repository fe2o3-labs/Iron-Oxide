-- Accounts. Sign-in identities live in their own tables (#5: passkeys, oauth_identities,
-- sessions), so a user row holds no email and no provider subject.
CREATE TYPE user_plan AS ENUM ('free', 'pro');

CREATE TABLE users (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    created_at   timestamptz NOT NULL DEFAULT now(),
    plan         user_plan   NOT NULL DEFAULT 'free',
    display_name text
);
