-- The active program is never an archived one (#19): an archived program cannot be made active,
-- and the active program cannot be archived. The server checks both under a lock on the program's
-- row (`active_program::set`, `programs::set_archived`); these triggers keep the rule for every
-- writer, concurrent ones included.
--
-- Errors are check violations naming a constraint, which the repository turns into
-- `RepoError::ProgramArchived` and `RepoError::ProgramActive`.

-- Setting the active program: FOR SHARE waits for a concurrent archive of the same program to
-- commit or roll back, then reads its outcome. Only the user's own program is looked at, so an id of
-- someone else's program falls through to the composite foreign key (not found), whatever its state.
CREATE FUNCTION active_program_not_archived() RETURNS trigger
    LANGUAGE plpgsql AS $$
DECLARE
    is_archived boolean;
BEGIN
    SELECT archived INTO is_archived FROM programs
        WHERE id = NEW.program_id AND user_id = NEW.user_id
        FOR SHARE;
    IF is_archived THEN
        RAISE EXCEPTION 'an archived program cannot be the active program'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'active_program_not_archived';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER active_program_not_archived BEFORE INSERT OR UPDATE OF program_id ON active_program
    FOR EACH ROW EXECUTE FUNCTION active_program_not_archived();

-- Archiving a program: the UPDATE holds the program's row lock, so a concurrent activation waits
-- in the trigger above; one that committed first is visible here (each statement of a volatile
-- function reads the latest committed rows).
CREATE FUNCTION program_not_archived_while_active() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM active_program WHERE program_id = NEW.id) THEN
        RAISE EXCEPTION 'the active program cannot be archived'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'programs_active_not_archived';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER programs_active_not_archived BEFORE UPDATE OF archived ON programs
    FOR EACH ROW WHEN (NEW.archived AND NOT OLD.archived)
    EXECUTE FUNCTION program_not_archived_while_active();
