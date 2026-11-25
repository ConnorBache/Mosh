CREATE TABLE native_volumes(id INTEGER PRIMARY KEY, identity TEXT NOT NULL UNIQUE);
INSERT INTO native_volumes(identity)
    SELECT DISTINCT mosh_native_volume(native_id) FROM objects WHERE native_id IS NOT NULL;
CREATE TABLE objects_next(
    key INTEGER PRIMARY KEY AUTOINCREMENT CHECK(key>0),
    name TEXT NOT NULL,
    path_folder INTEGER REFERENCES path_folders(id) CHECK(path_folder IS NULL OR path_literal IS NULL),
    native_ref BLOB,
    native_volume INTEGER REFERENCES native_volumes(id),
    created INTEGER,
    scan_stamp BLOB,
    parent_key INTEGER REFERENCES objects(key),
    tag_set INTEGER REFERENCES tag_sets(id),
    path_literal TEXT,
    kind_code INTEGER NOT NULL DEFAULT 0 CHECK(kind_code BETWEEN 0 AND 2),
    revision INTEGER NOT NULL DEFAULT 1,
    missing INTEGER NOT NULL DEFAULT 0,
    secret INTEGER NOT NULL DEFAULT 0,
    reparse INTEGER NOT NULL DEFAULT 0,
    legacy_id TEXT,
    mime TEXT,
    link_target TEXT,
    hash TEXT,
    git_root TEXT,
    git_status TEXT,
    git_author TEXT,
    git_committed INTEGER,
    id TEXT GENERATED ALWAYS AS (coalesce(legacy_id,mosh_object_id(key))) VIRTUAL,
    parent TEXT GENERATED ALWAYS AS (mosh_public_id(parent_key)) VIRTUAL,
    native_id TEXT GENERATED ALWAYS AS (mosh_native_identity(native_volume,native_ref)) VIRTUAL,
    path TEXT GENERATED ALWAYS AS (mosh_expand_path(path_folder,name,path_literal)) VIRTUAL,
    kind TEXT GENERATED ALWAYS AS (CASE kind_code WHEN 0 THEN 'file' WHEN 1 THEN 'coll' WHEN 2 THEN 'marriage' END) VIRTUAL,
    extension TEXT GENERATED ALWAYS AS (CASE WHEN path_folder IS NOT NULL OR path_literal IS NOT NULL
        THEN mosh_file_extension(name,kind_code) END) VIRTUAL,
    CHECK((native_volume IS NULL)=(native_ref IS NULL))
);
INSERT INTO objects_next(key,name,path_folder,native_ref,native_volume,created,scan_stamp,parent_key,tag_set,
    path_literal,kind_code,revision,missing,secret,reparse,legacy_id,mime,link_target,hash,
    git_root,git_status,git_author,git_committed)
SELECT o.key,o.name,o.path_folder,mosh_native_ref(o.native_id),v.id,o.created,
    CASE WHEN o.path IS NOT NULL THEN mosh_scan_stamp(o.size,o.modified,o.changed) END,p.key,o.tag_set,
    o.path_literal,o.kind_code,o.revision,o.missing,o.secret,o.reparse,o.legacy_id,o.mime,o.link_target,o.hash,
    o.git_root,o.git_status,o.git_author,o.git_committed
FROM objects o LEFT JOIN objects p ON p.id=o.parent
LEFT JOIN native_volumes v ON v.identity=mosh_native_volume(o.native_id);
