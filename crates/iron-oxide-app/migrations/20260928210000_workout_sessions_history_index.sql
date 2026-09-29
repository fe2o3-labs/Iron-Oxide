-- The history list (#20): a user's ended sessions, most recently finished first, paged on
-- (finished_at, id). In-progress sessions have no `finished_at` and are not part of it.
CREATE INDEX workout_sessions_history_idx ON workout_sessions (user_id, finished_at DESC, id DESC)
    WHERE status <> 'in_progress';
