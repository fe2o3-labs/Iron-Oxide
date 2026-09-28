-- Training data: settings, training maxes, programs and their immutable versions, the active
-- program, workout sessions and logged sets. See docs/database.md.
--
-- Server-side ids default to Postgres 18's `uuidv7()` (time-ordered, #65); client-generated ids
-- (sessions, sets, creation ids) are v7 too, from the domain's constructor.
--
-- Isolation rules (data isolation is the app's first security property):
-- - Every user-owned table has `user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE` and an
--   index that starts with `user_id` (built-in programs are the only rows without an owner).
-- - A row that points at another user-owned row does so through a composite foreign key that
--   includes `user_id`, so it can never point at another user's row, whatever the application does.
-- - `user_id` never changes once written (trigger `forbid_owner_change`).
--
-- Workout tables are called `workout_sessions`/`workout_sets`: `sessions` is the sign-in session
-- table of #5.
--
-- Value ranges mirror the domain types (iron-oxide-domain): `Reps` and set indexes are u16,
-- `Seconds` is u32, `Weight` is an exact number of nanograms up to 2000 kg, and exercise/day ids are
-- slugs of at most 64 characters.

-- A slug id (`ExerciseId`, `DayId`, built-in program id): 1 to 64 lowercase ASCII letters and
-- digits, in words separated by single hyphens.
CREATE FUNCTION is_slug(value text) RETURNS boolean
    LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
    RETURN octet_length(value) BETWEEN 1 AND 64 AND value ~ '^[a-z0-9]+(-[a-z0-9]+)*$';

-- A `Weight`: whole nanograms from 0 to 2000 kg (2000 * 10^12 ng).
CREATE FUNCTION is_weight_ng(value bigint) RETURNS boolean
    LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
    RETURN value BETWEEN 0 AND 2000000000000000;

-- Rejects any change of `user_id`: a row never moves to another user.
CREATE FUNCTION forbid_owner_change() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.user_id IS DISTINCT FROM OLD.user_id THEN
        RAISE EXCEPTION 'the owner of a % row cannot change', TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

-- Rejects a direct DELETE (row trigger) or a TRUNCATE (statement trigger); only deletes cascading
-- from a foreign key (a deleted user) pass.
-- pg_trigger_depth() is 1 for a trigger fired by a statement and at least 2 for one fired from
-- the referential action of a cascading foreign key.
CREATE FUNCTION forbid_direct_delete() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF pg_trigger_depth() < 2 THEN
        RAISE EXCEPTION '% rows are only deleted by cascade', TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN OLD;
END;
$$;

-- ---------------------------------------------------------------------------------------------
-- Settings: one row per user, created on the first save. Until then the app uses the column
-- defaults below (mirrored in the repository, and tested to match).

CREATE TABLE user_settings (
    user_id         uuid        PRIMARY KEY REFERENCES users ON DELETE CASCADE,
    unit            text        NOT NULL DEFAULT 'kg' CHECK (unit IN ('kg', 'lb')),
    bar_weight_ng   bigint      NOT NULL DEFAULT 20000000000000 CHECK (is_weight_ng(bar_weight_ng)),
    -- The domain's `PlateInventory` JSON (an array of {plate, pairs}, at most 16 sizes). Validated
    -- by the domain before it is written.
    plate_inventory jsonb       NOT NULL DEFAULT '[]'
        CHECK (jsonb_typeof(plate_inventory) = 'array' AND jsonb_array_length(plate_inventory) <= 16),
    default_rest_s  bigint      NOT NULL DEFAULT 120 CHECK (default_rest_s BETWEEN 0 AND 4294967295),
    sound_enabled   boolean     NOT NULL DEFAULT true,
    updated_at      timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER user_settings_owner BEFORE UPDATE OF user_id ON user_settings
    FOR EACH ROW EXECUTE FUNCTION forbid_owner_change();

-- Training maxes are per user and per exercise, not part of a program (#56). `set_at` is when the
-- lifter entered the value: the progression engine (#57) replays the history after it, so the
-- engine's computed training max is never written back here without moving `set_at`.
CREATE TABLE training_maxes (
    user_id     uuid        NOT NULL REFERENCES users ON DELETE CASCADE,
    exercise_id text        NOT NULL CHECK (is_slug(exercise_id)),
    weight_ng   bigint      NOT NULL CHECK (is_weight_ng(weight_ng)),
    set_at      timestamptz NOT NULL,
    PRIMARY KEY (user_id, exercise_id)
);

CREATE TRIGGER training_maxes_owner BEFORE UPDATE OF user_id ON training_maxes
    FOR EACH ROW EXECUTE FUNCTION forbid_owner_change();

-- ---------------------------------------------------------------------------------------------
-- Programs. A built-in program has no owner (`user_id IS NULL`) and a `source_builtin_id`; it is
-- seeded at startup and read by everyone. A user's program always has an owner; one copied from a
-- built-in keeps the built-in's id in `source_builtin_id`. Programs are archived, never deleted
-- (sessions stay linked to their versions), except by deleting the account (enforced by a trigger).
-- `creation_id` is the client's idempotency key for the request that created a user's program, so
-- a retried create or copy returns the program it already made instead of making a second one.

CREATE TABLE programs (
    id                uuid        PRIMARY KEY DEFAULT uuidv7(),
    user_id           uuid        REFERENCES users ON DELETE CASCADE,
    creation_id       uuid,
    source_builtin_id text        CHECK (is_slug(source_builtin_id)),
    name              text        NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
    archived          boolean     NOT NULL DEFAULT false,
    created_at        timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT programs_owner_or_builtin CHECK (user_id IS NOT NULL OR source_builtin_id IS NOT NULL),
    -- Every user program has a creation id; built-ins (seeded by the server) have none.
    CONSTRAINT programs_creation_id_iff_owned CHECK ((user_id IS NULL) = (creation_id IS NULL)),
    CONSTRAINT programs_user_id_creation_id_key UNIQUE (user_id, creation_id),
    -- Target of the composite foreign keys that tie child rows to the same owner.
    CONSTRAINT programs_id_user_id_key UNIQUE (id, user_id)
);

CREATE INDEX programs_user_id_idx ON programs (user_id, created_at);
CREATE UNIQUE INDEX programs_builtin_key ON programs (source_builtin_id) WHERE user_id IS NULL;

CREATE TRIGGER programs_owner BEFORE UPDATE OF user_id ON programs
    FOR EACH ROW EXECUTE FUNCTION forbid_owner_change();

-- Programs are archived, never deleted: only the cascade from a deleted user removes them.
CREATE TRIGGER programs_no_direct_delete BEFORE DELETE ON programs
    FOR EACH ROW EXECUTE FUNCTION forbid_direct_delete();

-- Immutable versions of a program: an upload or edit adds a version, nothing updates one.
-- `user_id` is copied from the program by a trigger (the caller cannot choose it), so a version
-- always has its program's owner, including NULL for built-ins.
CREATE TABLE program_versions (
    id         uuid        PRIMARY KEY DEFAULT uuidv7(),
    program_id uuid        NOT NULL REFERENCES programs ON DELETE CASCADE,
    user_id    uuid        REFERENCES users ON DELETE CASCADE,
    version    integer     NOT NULL CHECK (version >= 1),
    -- The program JSON document (#56). The domain validates it before it is stored.
    document   jsonb       NOT NULL CHECK (
        jsonb_typeof(document) = 'object'
        -- IS NOT DISTINCT FROM: a missing key gives NULL, which a plain `=` would let through.
        AND jsonb_typeof(document -> 'schema_version') IS NOT DISTINCT FROM 'number'
        AND octet_length(document::text) <= 1048576
    ),
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT program_versions_program_id_version_key UNIQUE (program_id, version),
    CONSTRAINT program_versions_id_user_id_key UNIQUE (id, user_id),
    -- Checked whenever user_id is set (MATCH SIMPLE skips NULL, which only built-ins have).
    CONSTRAINT program_versions_program_owner_fkey FOREIGN KEY (program_id, user_id)
        REFERENCES programs (id, user_id) ON DELETE CASCADE
);

CREATE INDEX program_versions_user_id_idx ON program_versions (user_id);

CREATE FUNCTION program_versions_set_owner() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    NEW.user_id := (SELECT user_id FROM programs WHERE id = NEW.program_id);
    RETURN NEW;
END;
$$;

CREATE TRIGGER program_versions_set_owner BEFORE INSERT ON program_versions
    FOR EACH ROW EXECUTE FUNCTION program_versions_set_owner();

CREATE FUNCTION forbid_update() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% rows are immutable', TG_TABLE_NAME
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;

CREATE TRIGGER program_versions_immutable BEFORE UPDATE ON program_versions
    FOR EACH ROW EXECUTE FUNCTION forbid_update();

-- No direct delete either (a deleted version would free its number for other content): only the
-- cascade from a deleted user removes versions.
CREATE TRIGGER program_versions_no_direct_delete BEFORE DELETE ON program_versions
    FOR EACH ROW EXECUTE FUNCTION forbid_direct_delete();

-- The program a user trains with. The composite key means it can only be one of the user's own
-- programs (never a built-in: copy it first, #19).
CREATE TABLE active_program (
    user_id    uuid        PRIMARY KEY REFERENCES users ON DELETE CASCADE,
    program_id uuid        NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT active_program_program_owner_fkey FOREIGN KEY (program_id, user_id)
        REFERENCES programs (id, user_id) ON DELETE CASCADE
);

CREATE INDEX active_program_program_id_idx ON active_program (program_id);

CREATE TRIGGER active_program_owner BEFORE UPDATE OF user_id ON active_program
    FOR EACH ROW EXECUTE FUNCTION forbid_owner_change();

-- ---------------------------------------------------------------------------------------------
-- Workout sessions and sets. Ids are generated on the client (idempotent retries, #54), so they
-- are only unique per user: the primary key is (user_id, id). Another user's id is then exactly
-- like a free one (no "taken" signal), and two users may use the same UUID independently.

CREATE TABLE workout_sessions (
    id                 uuid        NOT NULL,
    user_id            uuid        NOT NULL REFERENCES users ON DELETE CASCADE,
    program_version_id uuid        NOT NULL,
    day_id             text        NOT NULL CHECK (is_slug(day_id)),
    status             text        NOT NULL
        CHECK (status IN ('in_progress', 'completed', 'skipped', 'abandoned')),
    started_at         timestamptz NOT NULL,
    finished_at        timestamptz,
    CONSTRAINT workout_sessions_finished_iff_ended
        CHECK ((status = 'in_progress') = (finished_at IS NULL)),
    CONSTRAINT workout_sessions_finished_after_start CHECK (finished_at >= started_at),
    CONSTRAINT workout_sessions_pkey PRIMARY KEY (user_id, id),
    -- Only one of the user's own program versions (never a built-in's, never another user's).
    -- NO ACTION (checked at the end of the statement), not RESTRICT, so deleting a user can
    -- cascade to both this table and program_versions in one statement.
    CONSTRAINT workout_sessions_program_version_owner_fkey FOREIGN KEY (program_version_id, user_id)
        REFERENCES program_versions (id, user_id)
);

CREATE INDEX workout_sessions_user_id_idx ON workout_sessions (user_id, started_at DESC, id DESC);
CREATE INDEX workout_sessions_program_version_id_idx ON workout_sessions (program_version_id);

CREATE TRIGGER workout_sessions_owner BEFORE UPDATE OF user_id ON workout_sessions
    FOR EACH ROW EXECUTE FUNCTION forbid_owner_change();

CREATE TABLE workout_sets (
    id           uuid        NOT NULL,
    session_id   uuid        NOT NULL,
    user_id      uuid        NOT NULL REFERENCES users ON DELETE CASCADE,
    exercise_id  text        NOT NULL CHECK (is_slug(exercise_id)),
    set_index    integer     NOT NULL CHECK (set_index BETWEEN 0 AND 65535),
    reps         integer     NOT NULL CHECK (reps BETWEEN 0 AND 65535),
    weight_ng    bigint      CHECK (is_weight_ng(weight_ng)),
    duration_s   bigint      CHECK (duration_s BETWEEN 0 AND 4294967295),
    warmup       boolean     NOT NULL,
    completed_at timestamptz NOT NULL,
    CONSTRAINT workout_sets_pkey PRIMARY KEY (user_id, id),
    -- A set belongs to one of its owner's sessions, never another user's.
    CONSTRAINT workout_sets_session_owner_fkey FOREIGN KEY (user_id, session_id)
        REFERENCES workout_sessions (user_id, id) ON DELETE CASCADE
);

-- Serves the per-exercise history: progression input after a training max's `set_at` (#57) and
-- the exercise charts (#20).
CREATE INDEX workout_sets_user_id_idx ON workout_sets (user_id, exercise_id, completed_at);
CREATE INDEX workout_sets_session_id_idx ON workout_sets (user_id, session_id, completed_at);

CREATE TRIGGER workout_sets_owner BEFORE UPDATE OF user_id ON workout_sets
    FOR EACH ROW EXECUTE FUNCTION forbid_owner_change();

-- ---------------------------------------------------------------------------------------------
-- No TRUNCATE: it skips row-level DELETE triggers, and `TRUNCATE ... CASCADE` on one table would
-- empty every user's data at once. The app never truncates; accounts are deleted one by one.
CREATE TRIGGER users_no_truncate BEFORE TRUNCATE ON users
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();
CREATE TRIGGER user_settings_no_truncate BEFORE TRUNCATE ON user_settings
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();
CREATE TRIGGER training_maxes_no_truncate BEFORE TRUNCATE ON training_maxes
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();
CREATE TRIGGER programs_no_truncate BEFORE TRUNCATE ON programs
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();
CREATE TRIGGER program_versions_no_truncate BEFORE TRUNCATE ON program_versions
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();
CREATE TRIGGER active_program_no_truncate BEFORE TRUNCATE ON active_program
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();
CREATE TRIGGER workout_sessions_no_truncate BEFORE TRUNCATE ON workout_sessions
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();
CREATE TRIGGER workout_sets_no_truncate BEFORE TRUNCATE ON workout_sets
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_direct_delete();
