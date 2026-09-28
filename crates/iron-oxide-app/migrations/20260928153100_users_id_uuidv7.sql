-- New user ids are UUIDv7 (#65), like every other row id. Existing ids keep their value.
ALTER TABLE users ALTER COLUMN id SET DEFAULT uuidv7();
