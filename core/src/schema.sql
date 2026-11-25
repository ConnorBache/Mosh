CREATE TABLE people(
    id TEXT PRIMARY KEY, superior TEXT REFERENCES people(id)
) WITHOUT ROWID;
INSERT INTO people VALUES('local',NULL);
CREATE TABLE objects(
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('file','coll','marriage')),
    path TEXT,
    parent TEXT,
    name TEXT NOT NULL,
    extension TEXT,
    mime TEXT,
    size INTEGER NOT NULL DEFAULT 0,
    allocated INTEGER,
    created INTEGER,
    modified INTEGER,
    accessed INTEGER,
    changed INTEGER,
    attributes INTEGER,
    native_id TEXT,
    volume TEXT,
    filesystem TEXT,
    hardlinks INTEGER,
    reparse INTEGER NOT NULL DEFAULT 0,
    link_target TEXT,
    hash TEXT,
    content TEXT NOT NULL DEFAULT '',
    revision INTEGER NOT NULL DEFAULT 1,
    seen TEXT,
    missing INTEGER NOT NULL DEFAULT 0,
    secret INTEGER NOT NULL DEFAULT 0,
    git_root TEXT,
    git_status TEXT,
    git_author TEXT,
    git_committed INTEGER
);
CREATE UNIQUE INDEX live_path ON objects(path) WHERE missing=0 AND path IS NOT NULL;
CREATE INDEX object_path ON objects(path,missing);
CREATE INDEX object_parent ON objects(parent,missing,name COLLATE NOCASE,id);
CREATE INDEX object_native ON objects(native_id,created,missing) WHERE native_id IS NOT NULL;
CREATE INDEX object_ext ON objects(extension,missing);
CREATE INDEX object_modified ON objects(modified,missing);
CREATE INDEX object_size ON objects(size,missing) WHERE kind='file';
CREATE INDEX object_hash ON objects(hash) WHERE hash IS NOT NULL AND missing=0;
CREATE INDEX object_git ON objects(git_root,git_status);
CREATE INDEX object_secret ON objects(path) WHERE secret=1 AND missing=0;
CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE roots(path TEXT PRIMARY KEY,scanned INTEGER) WITHOUT ROWID;
CREATE TABLE owners(
    object TEXT NOT NULL REFERENCES objects(id), person TEXT NOT NULL REFERENCES people(id),
    PRIMARY KEY(object,person)
) WITHOUT ROWID;
CREATE TABLE guests(
    object TEXT NOT NULL REFERENCES objects(id), person TEXT NOT NULL REFERENCES people(id),
    permission TEXT NOT NULL CHECK(permission IN ('read','edit')),
    PRIMARY KEY(object,person)
) WITHOUT ROWID;
CREATE TABLE tag_dictionary(id INTEGER PRIMARY KEY, name TEXT UNIQUE NOT NULL);
CREATE TABLE tag_aliases(alias TEXT PRIMARY KEY, tag INTEGER NOT NULL REFERENCES tag_dictionary(id) ON DELETE CASCADE) WITHOUT ROWID;
CREATE TABLE tag_assignments(
    object TEXT NOT NULL REFERENCES objects(id),
    tag INTEGER NOT NULL REFERENCES tag_dictionary(id) ON DELETE CASCADE,
    origin TEXT NOT NULL CHECK(origin IN ('user','automatic','filesystem','git','suggested')),
    confidence REAL NOT NULL CHECK(confidence BETWEEN 0 AND 1),
    evidence TEXT,
    PRIMARY KEY(object,tag,origin)
) WITHOUT ROWID;
CREATE INDEX tag_lookup ON tag_assignments(tag,object,origin);
CREATE TABLE rules(tag TEXT NOT NULL,query TEXT NOT NULL,PRIMARY KEY(tag,query)) WITHOUT ROWID;
CREATE TABLE paint(
    object TEXT PRIMARY KEY REFERENCES objects(id),
    level TEXT NOT NULL CHECK(level IN ('minimal','standard','detailed')),
    body TEXT NOT NULL,
    revision INTEGER NOT NULL,
    locked INTEGER NOT NULL,
    origin TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE edges(
    container TEXT NOT NULL REFERENCES objects(id),
    target TEXT NOT NULL REFERENCES objects(id),
    kind TEXT NOT NULL CHECK(kind IN ('parent','reference','related')),
    selector TEXT NOT NULL DEFAULT '',
    PRIMARY KEY(container,target,kind,selector)
) WITHOUT ROWID;
CREATE INDEX reverse_edges ON edges(target,kind,container);
CREATE TABLE collection_queries(
    container TEXT NOT NULL REFERENCES objects(id), query TEXT NOT NULL,
    PRIMARY KEY(container,query)
) WITHOUT ROWID;
CREATE TABLE saved_searches(
    actor TEXT NOT NULL REFERENCES people(id), name TEXT NOT NULL, query TEXT NOT NULL,
    PRIMARY KEY(actor,name)
) WITHOUT ROWID;
CREATE TABLE audit(
    seq INTEGER PRIMARY KEY, time INTEGER NOT NULL,
    object TEXT REFERENCES objects(id), action TEXT NOT NULL,
    actor TEXT, previous TEXT, current TEXT, outcome TEXT NOT NULL
);
CREATE INDEX object_audit ON audit(object,seq DESC);
CREATE TRIGGER audit_no_updates BEFORE UPDATE ON audit BEGIN SELECT RAISE(ABORT,'audit is append-only'); END;
CREATE TRIGGER audit_no_deletes BEFORE DELETE ON audit BEGIN SELECT RAISE(ABORT,'audit is append-only'); END;
CREATE TABLE plans(
    id TEXT PRIMARY KEY, actor TEXT NOT NULL REFERENCES people(id), action TEXT NOT NULL,
    state TEXT NOT NULL, created INTEGER NOT NULL, manifest TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE plan_steps(
    plan TEXT NOT NULL REFERENCES plans(id), step INTEGER NOT NULL,
    state TEXT NOT NULL, details TEXT,
    PRIMARY KEY(plan,step)
) WITHOUT ROWID;
CREATE TABLE jobs(
    id INTEGER PRIMARY KEY, object TEXT NOT NULL REFERENCES objects(id), revision INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'queued', attempts INTEGER NOT NULL DEFAULT 0,
    created INTEGER NOT NULL, prompt_tokens INTEGER NOT NULL DEFAULT 0,
    reply_tokens INTEGER NOT NULL DEFAULT 0, elapsed_ms INTEGER NOT NULL DEFAULT 0,
    provider TEXT, error TEXT, suggestion TEXT, UNIQUE(object,revision)
);
CREATE INDEX queued_jobs ON jobs(state,created,id);
CREATE TABLE repositories(path TEXT PRIMARY KEY, name TEXT NOT NULL, branch TEXT) WITHOUT ROWID;
CREATE INDEX repository_names ON repositories(name COLLATE NOCASE);
CREATE VIRTUAL TABLE ledger USING fts5(object UNINDEXED,name,path,content,paint,tokenize='unicode61');
CREATE TRIGGER ledger_update AFTER UPDATE OF path,name,content,missing ON objects
WHEN OLD.path IS NOT NEW.path OR OLD.name IS NOT NEW.name OR OLD.content IS NOT NEW.content OR OLD.missing<>NEW.missing BEGIN
    DELETE FROM ledger WHERE rowid=OLD.rowid;
    INSERT INTO ledger(rowid,object,name,path,content,paint)
        SELECT NEW.rowid,NEW.id,NEW.name,coalesce(NEW.path,''),NEW.content,
               coalesce((SELECT body FROM paint WHERE object=NEW.id),'') WHERE NEW.missing=0;
END;
CREATE TRIGGER ledger_insert AFTER INSERT ON objects WHEN NEW.missing=0 BEGIN
    INSERT INTO ledger(rowid,object,name,path,content,paint)
    VALUES(NEW.rowid,NEW.id,NEW.name,coalesce(NEW.path,''),NEW.content,'');
END;
CREATE TRIGGER paint_insert AFTER INSERT ON paint BEGIN
    UPDATE ledger SET paint=NEW.body WHERE rowid=(SELECT rowid FROM objects WHERE id=NEW.object);
END;
CREATE TRIGGER paint_update AFTER UPDATE OF body ON paint WHEN NEW.body<>OLD.body BEGIN
    UPDATE ledger SET paint=NEW.body WHERE rowid=(SELECT rowid FROM objects WHERE id=NEW.object);
END;
CREATE TRIGGER paint_delete AFTER DELETE ON paint BEGIN
    UPDATE ledger SET paint='' WHERE rowid=(SELECT rowid FROM objects WHERE id=OLD.object);
END;
