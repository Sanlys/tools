-- game-mgr stats server, initial schema (PLAN.md §8.1)

CREATE TABLE users (
  id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  sub          text NOT NULL UNIQUE,               -- OIDC subject; auto-provisioned
  display_name text,
  created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE profiles (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  owner_user_id uuid NOT NULL REFERENCES users(id),
  name          text NOT NULL,
  created_at    timestamptz NOT NULL DEFAULT now()
);

-- immutable audit trail of profile ownership changes
CREATE TABLE profile_transfers (
  id             bigserial PRIMARY KEY,
  profile_id     uuid NOT NULL REFERENCES profiles(id),
  from_user_id   uuid NOT NULL REFERENCES users(id),
  to_user_id     uuid NOT NULL REFERENCES users(id),
  transferred_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE machines (
  id                  uuid PRIMARY KEY,            -- client-generated, stable per machine
  name                text NOT NULL,
  os                  text,
  client_version      text,
  registered_by       uuid NOT NULL REFERENCES users(id),
  syncthing_device_id text,                        -- mesh peer discovery (PLAN.md §5)
  created_at          timestamptz NOT NULL DEFAULT now(),
  last_seen_at        timestamptz
);

CREATE TABLE games (                               -- catalog snapshot pushed by clients
  id             text PRIMARY KEY,                 -- GameId slug
  title          text NOT NULL,
  class          text NOT NULL,
  latest_version text NOT NULL,
  updated_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE installs (
  machine_id   uuid NOT NULL REFERENCES machines(id),
  game_id      text NOT NULL REFERENCES games(id),
  version      text NOT NULL,
  state        text NOT NULL CHECK (state IN
               ('installing','installed','outdated','failed','uninstalled')),
  proton       text,
  installed_at timestamptz,
  updated_at   timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (machine_id, game_id)
);

CREATE TABLE sessions (
  id         uuid PRIMARY KEY,                     -- client-generated => idempotent ingest
  machine_id uuid NOT NULL REFERENCES machines(id),
  profile_id uuid NOT NULL REFERENCES profiles(id),
  game_id    text NOT NULL REFERENCES games(id),
  started_at timestamptz NOT NULL,
  ended_at   timestamptz NOT NULL,
  duration_s integer NOT NULL CHECK (duration_s >= 0)
);
CREATE INDEX sessions_game_time    ON sessions (game_id, started_at);
CREATE INDEX sessions_profile_time ON sessions (profile_id, started_at);
CREATE INDEX sessions_machine_time ON sessions (machine_id, started_at);

CREATE TABLE sync_status (
  machine_id     uuid NOT NULL REFERENCES machines(id),
  game_id        text NOT NULL REFERENCES games(id),
  folder_id      text NOT NULL,
  state          text NOT NULL,                    -- idle|syncing|error|paused
  completion_pct real,
  last_synced_at timestamptz,
  conflict_count int  NOT NULL DEFAULT 0,
  conflict_files jsonb NOT NULL DEFAULT '[]',
  reported_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (machine_id, folder_id)
);

CREATE TABLE events (
  id              bigserial PRIMARY KEY,
  client_event_id uuid NOT NULL UNIQUE,            -- idempotent ingest
  machine_id      uuid NOT NULL REFERENCES machines(id),
  profile_id      uuid REFERENCES profiles(id),    -- null for machine-level events
  game_id         text REFERENCES games(id),
  kind            text NOT NULL,
  payload         jsonb NOT NULL DEFAULT '{}',
  occurred_at     timestamptz NOT NULL
);
CREATE INDEX events_kind_time ON events (kind, occurred_at);
