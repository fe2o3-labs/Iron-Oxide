-- At most one session in progress per user (#18). `start_session` checks it first and answers
-- 409; this index makes it hold under concurrent requests too (two devices starting different
-- sessions at the same instant): the second insert fails with a unique violation, also a 409.
-- It starts with `user_id`, as every unique key of a user-owned table must (docs/database.md).
CREATE UNIQUE INDEX workout_sessions_one_in_progress_idx
    ON workout_sessions (user_id) WHERE status = 'in_progress';
