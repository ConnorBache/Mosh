CREATE TABLE objects_next(
    key INTEGER PRIMARY KEY AUTOINCREMENT CHECK(key>0),
    legacy_id TEXT,
    id TEXT GENERATED ALWAYS AS (coalesce(legacy_id,mosh_object_id(key))) VIRTUAL UNIQUE,
    path_literal TEXT,
    parent TEXT,
    name TEXT NOT NULL,
    mime TEXT,
    size INTEGER NOT NULL DEFAULT 0,
    created INTEGER,
    modified INTEGER,
    changed INTEGER,
    native_id BLOB,
    reparse INTEGER NOT NULL DEFAULT 0,
    link_target TEXT,
    hash TEXT,
    revision INTEGER NOT NULL DEFAULT 1,
    missing INTEGER NOT NULL DEFAULT 0,
    secret INTEGER NOT NULL DEFAULT 0,
    git_root TEXT,
    git_status TEXT,
    git_author TEXT,
    git_committed INTEGER,
    path_folder INTEGER REFERENCES path_folders(id) CHECK(path_folder IS NULL OR path_literal IS NULL),
    kind_code INTEGER NOT NULL DEFAULT 0 CHECK(kind_code BETWEEN 0 AND 2),
    tag_set INTEGER REFERENCES tag_sets(id),
    path TEXT GENERATED ALWAYS AS (mosh_expand_path(path_folder,name,path_literal)) VIRTUAL,
    kind TEXT GENERATED ALWAYS AS (CASE kind_code WHEN 0 THEN 'file' WHEN 1 THEN 'coll' WHEN 2 THEN 'marriage' END) VIRTUAL,
    extension TEXT GENERATED ALWAYS AS (CASE WHEN path_folder IS NOT NULL OR path_literal IS NOT NULL
        THEN mosh_file_extension(name,kind_code) END) VIRTUAL,
    accessed INTEGER GENERATED ALWAYS AS (NULL) VIRTUAL
);
INSERT INTO objects_next(key,legacy_id,path_literal,parent,name,mime,size,created,modified,changed,native_id,
    reparse,link_target,hash,revision,missing,secret,git_root,git_status,git_author,git_committed,path_folder,kind_code,tag_set)
SELECT k.key,CASE WHEN o.id<>mosh_object_id(k.key) THEN o.id END,o.path_literal,o.parent,o.name,o.mime,o.size,
    o.created,o.modified,o.changed,o.native_id,o.reparse,o.link_target,o.hash,o.revision,o.missing,o.secret,
    o.git_root,o.git_status,o.git_author,o.git_committed,o.path_folder,o.kind_code,k.tag_set
FROM objects o JOIN object_keys k ON k.object=o.id;
