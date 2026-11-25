CREATE TABLE nested_moshes(
    child TEXT PRIMARY KEY, name TEXT NOT NULL, paint TEXT NOT NULL,
    tags TEXT NOT NULL, attachment TEXT NOT NULL, root TEXT NOT NULL UNIQUE,
    native_id TEXT, native_created INTEGER
) WITHOUT ROWID;
CREATE TABLE nested_discovery(
    child TEXT NOT NULL REFERENCES nested_moshes(child) ON DELETE CASCADE,
    actor TEXT NOT NULL REFERENCES people(id),
    PRIMARY KEY(child,actor)
) WITHOUT ROWID;
CREATE TABLE handoff_grants(
    parent TEXT NOT NULL, actor TEXT NOT NULL REFERENCES people(id),
    PRIMARY KEY(parent,actor)
) WITHOUT ROWID;
CREATE TABLE nested_suspended(
    child TEXT REFERENCES nested_moshes(child) ON DELETE SET NULL,
    object_key INTEGER NOT NULL PRIMARY KEY REFERENCES object_keys(key)
) WITHOUT ROWID;
