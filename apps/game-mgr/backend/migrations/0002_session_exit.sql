-- Sessions are first-class browsable records: exit status for crash
-- detection (PLAN.md §8.1).

ALTER TABLE sessions ADD COLUMN exit_code integer;
ALTER TABLE sessions ADD COLUMN end_reason text NOT NULL DEFAULT 'exited';
