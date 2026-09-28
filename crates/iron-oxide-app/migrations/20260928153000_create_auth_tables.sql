-- Sign-in (#5): passkeys, Google identities, server-side sessions and one-time ceremony state.
-- Every row that belongs to a user is deleted with the user (ON DELETE CASCADE, #22).
-- See docs/auth.md for the design.
--
-- Row ids are UUIDv7 (`uuidv7()`, Postgres 18; #65). Nothing secret or exposed to authenticators
-- is a UUIDv7: session ids, WebAuthn user handles and the OAuth state, nonce and PKCE verifier are
-- cryptographically random.
--
-- Like every user-owned table (#17, #58): `user_id` cascades from `users` and is indexed, it never
-- changes once written (trigger `forbid_owner_change`), and the table cannot be truncated
-- (`forbid_direct_delete` on TRUNCATE). Both functions come from the training schema migration.
-- Rows are deleted directly here (sign-out, used ceremonies, removed passkeys), so there is no
-- row-level delete guard.

-- The WebAuthn user handle of each user who has registered a passkey. Random (UUIDv4 from the OS
-- CSPRNG), never the user id: authenticators store it, and a UUIDv7 user id would tell them when
-- the account was created. One per user, the same for all their passkeys.
CREATE TABLE webauthn_user_handles (
    user_id     uuid        PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    user_handle uuid        NOT NULL UNIQUE,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER webauthn_user_handles_owner BEFORE UPDATE OF user_id ON webauthn_user_handles
    FOR EACH ROW EXECUTE FUNCTION forbid_owner_change();
CREATE TRIGGER webauthn_user_handles_no_truncate BEFORE TRUNCATE ON webauthn_user_handles
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();

-- WebAuthn credentials (passkeys). A user may have several.
CREATE TABLE passkeys (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    user_id         uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- The raw credential ID chosen by the authenticator. Unique across all users: a credential
    -- can never be registered to two accounts.
    credential_id   bytea       NOT NULL UNIQUE,
    -- The `webauthn_rs::prelude::Passkey`, serialized as JSON: public key, algorithm, counter,
    -- backup flags. The source of truth for verification; the columns below mirror it for display.
    passkey         jsonb       NOT NULL,
    sign_count      bigint      NOT NULL DEFAULT 0 CHECK (sign_count >= 0),
    backup_eligible boolean     NOT NULL,
    backup_state    boolean     NOT NULL,
    nickname        text        NOT NULL CHECK (char_length(nickname) BETWEEN 1 AND 64),
    created_at      timestamptz NOT NULL DEFAULT now(),
    last_used_at    timestamptz
);

CREATE INDEX passkeys_user_id_idx ON passkeys (user_id);
CREATE TRIGGER passkeys_owner BEFORE UPDATE OF user_id ON passkeys
    FOR EACH ROW EXECUTE FUNCTION forbid_owner_change();
CREATE TRIGGER passkeys_no_truncate BEFORE TRUNCATE ON passkeys
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();

-- External identities (Sign in with Google). Linked by the provider's stable subject (`sub`),
-- never by email.
CREATE TYPE oauth_provider AS ENUM ('google');

CREATE TABLE oauth_identities (
    id         uuid           PRIMARY KEY DEFAULT uuidv7(),
    user_id    uuid           NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    provider   oauth_provider NOT NULL,
    subject    text           NOT NULL CHECK (char_length(subject) BETWEEN 1 AND 255),
    created_at timestamptz    NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    -- One account per external identity.
    UNIQUE (provider, subject),
    -- At most one identity per provider per user.
    UNIQUE (user_id, provider)
);

CREATE TRIGGER oauth_identities_owner BEFORE UPDATE OF user_id ON oauth_identities
    FOR EACH ROW EXECUTE FUNCTION forbid_owner_change();
CREATE TRIGGER oauth_identities_no_truncate BEFORE TRUNCATE ON oauth_identities
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();

-- Server-side sessions (tower-sessions, through our own store in server/auth/session.rs).
-- The cookie holds a random session ID; only its SHA-256 is stored here, so a copy of this table
-- cannot be replayed as cookies. `user_id` is written once, when the signed-in session is created
-- (sign-in always starts a new row), so deleting the user deletes their sessions and "sign out
-- everywhere" is one DELETE. NULL for a signed-out session holding a ceremony.
CREATE TABLE sessions (
    id_hash    bytea       PRIMARY KEY CHECK (octet_length(id_hash) = 32),
    user_id    uuid        REFERENCES users (id) ON DELETE CASCADE,
    data       jsonb       NOT NULL,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);
CREATE INDEX sessions_user_id_idx ON sessions (user_id);
CREATE TRIGGER sessions_owner BEFORE UPDATE OF user_id ON sessions
    FOR EACH ROW EXECUTE FUNCTION forbid_owner_change();
CREATE TRIGGER sessions_no_truncate BEFORE TRUNCATE ON sessions
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();

-- In-flight sign-in ceremonies: the WebAuthn challenge state, or the Google state, nonce and PKCE
-- verifier. Short-lived and single-use: consumed with DELETE ... RETURNING, so two concurrent
-- attempts cannot both use the same one. The session holds the row's id.
CREATE TYPE auth_ceremony_kind AS ENUM (
    'passkey_sign_up',
    'passkey_sign_in',
    'passkey_add',
    'google_sign_in',
    'google_link'
);

CREATE TABLE auth_ceremonies (
    id         uuid               PRIMARY KEY,
    kind       auth_ceremony_kind NOT NULL,
    -- The signed-in user who started it (adding a passkey, linking Google); NULL otherwise.
    user_id    uuid               REFERENCES users (id) ON DELETE CASCADE,
    state      jsonb              NOT NULL,
    created_at timestamptz        NOT NULL DEFAULT now(),
    expires_at timestamptz        NOT NULL,
    CHECK ((kind IN ('passkey_add', 'google_link')) = (user_id IS NOT NULL))
);

CREATE INDEX auth_ceremonies_expires_at_idx ON auth_ceremonies (expires_at);
CREATE INDEX auth_ceremonies_user_id_idx ON auth_ceremonies (user_id);
-- Google callbacks find a ceremony by the `state` they carry (to discard it when it arrives in
-- another session).
CREATE INDEX auth_ceremonies_google_state_idx ON auth_ceremonies ((state ->> 'state'))
    WHERE kind IN ('google_sign_in', 'google_link');
CREATE TRIGGER auth_ceremonies_owner BEFORE UPDATE OF user_id ON auth_ceremonies
    FOR EACH ROW EXECUTE FUNCTION forbid_owner_change();
CREATE TRIGGER auth_ceremonies_no_truncate BEFORE TRUNCATE ON auth_ceremonies
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();
