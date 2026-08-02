-- Titles become server-stored data (PLAN.md §4.1): class-specific config and
-- the pinned artifact list live on the games row. Profiles become deletable:
-- a deleted profile takes its sessions/transfers with it; events keep the
-- row with profile_id nulled.

ALTER TABLE games ADD COLUMN config jsonb NOT NULL DEFAULT '{}';
ALTER TABLE games ADD COLUMN artifacts jsonb NOT NULL DEFAULT '[]';
ALTER TABLE games RENAME COLUMN latest_version TO version;

ALTER TABLE sessions DROP CONSTRAINT sessions_profile_id_fkey;
ALTER TABLE sessions ADD CONSTRAINT sessions_profile_id_fkey
  FOREIGN KEY (profile_id) REFERENCES profiles(id) ON DELETE CASCADE;

ALTER TABLE events DROP CONSTRAINT events_profile_id_fkey;
ALTER TABLE events ADD CONSTRAINT events_profile_id_fkey
  FOREIGN KEY (profile_id) REFERENCES profiles(id) ON DELETE SET NULL;

ALTER TABLE profile_transfers DROP CONSTRAINT profile_transfers_profile_id_fkey;
ALTER TABLE profile_transfers ADD CONSTRAINT profile_transfers_profile_id_fkey
  FOREIGN KEY (profile_id) REFERENCES profiles(id) ON DELETE CASCADE;
