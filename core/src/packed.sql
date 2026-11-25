CREATE TABLE tag_sets(
    id INTEGER PRIMARY KEY, digest BLOB NOT NULL UNIQUE, packed BLOB NOT NULL,
    confirmed INTEGER NOT NULL CHECK(confirmed>=0)
);
CREATE TABLE object_keys(
    key INTEGER PRIMARY KEY AUTOINCREMENT,
    object TEXT NOT NULL UNIQUE REFERENCES objects(id),
    tag_set INTEGER REFERENCES tag_sets(id)
);
CREATE INDEX tag_set_owners ON object_keys(tag_set) WHERE tag_set IS NOT NULL;
CREATE TABLE tag_blocks(
    tag INTEGER NOT NULL REFERENCES tag_dictionary(id),
    suggested INTEGER NOT NULL CHECK(suggested IN (0,1)),
    block INTEGER NOT NULL CHECK(block>=0),
    packed BLOB NOT NULL,
    PRIMARY KEY(tag,suggested,block)
) WITHOUT ROWID;
CREATE TABLE tag_details(
    object_key INTEGER NOT NULL REFERENCES object_keys(key),
    tag INTEGER NOT NULL REFERENCES tag_dictionary(id),
    origin INTEGER NOT NULL CHECK(origin IN (1,2,4,8,16)),
    confidence REAL NOT NULL CHECK(confidence BETWEEN 0 AND 1),
    evidence TEXT,
    PRIMARY KEY(object_key,tag,origin)
) WITHOUT ROWID;
CREATE TABLE tag_audit_members(
    event INTEGER PRIMARY KEY REFERENCES audit(seq),
    keys BLOB NOT NULL
);
CREATE TRIGGER tag_audit_no_updates BEFORE UPDATE ON tag_audit_members BEGIN SELECT RAISE(ABORT,'audit is append-only'); END;
CREATE TRIGGER tag_audit_no_deletes BEFORE DELETE ON tag_audit_members BEGIN SELECT RAISE(ABORT,'audit is append-only'); END;
