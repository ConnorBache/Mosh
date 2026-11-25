-- Keep only exact directory spellings in this dictionary. A directory move must
-- not silently change the recorded locations of files that have not been scanned.
CREATE TABLE path_folders(id INTEGER PRIMARY KEY, prefix TEXT NOT NULL UNIQUE);
CREATE TRIGGER path_folders_immutable BEFORE UPDATE ON path_folders
BEGIN SELECT RAISE(ABORT,'path directory spellings are immutable'); END;

DROP TRIGGER unique_live_path_insert;
DROP TRIGGER unique_live_path_update;
DROP TRIGGER ledger_insert;
DROP TRIGGER ledger_update;
DROP TRIGGER text_invalidate;
DROP TRIGGER paint_insert;
DROP TRIGGER paint_update;
DROP TRIGGER paint_delete;
DROP INDEX live_path_key;
DROP INDEX object_secret;
DROP INDEX object_ext;
DROP INDEX object_modified;
DROP INDEX object_size;

ALTER TABLE objects RENAME COLUMN path TO path_literal;
ALTER TABLE objects ADD COLUMN path_folder INTEGER REFERENCES path_folders(id)
    CHECK(path_folder IS NULL OR path_literal IS NULL);
ALTER TABLE objects ADD COLUMN path TEXT
    GENERATED ALWAYS AS (mosh_expand_path(path_folder,name,path_literal)) VIRTUAL;

INSERT INTO path_folders(prefix)
    SELECT DISTINCT mosh_path_prefix(path_literal,name) FROM objects WHERE kind='file'
    AND mosh_path_prefix(path_literal,name) IS NOT NULL;
UPDATE objects SET path_folder=path_folders.id,path_literal=NULL FROM path_folders
    WHERE kind='file' AND path_folders.prefix=mosh_path_prefix(objects.path_literal,objects.name);

ALTER TABLE objects RENAME COLUMN kind TO old_kind;
ALTER TABLE objects ADD COLUMN kind_code INTEGER NOT NULL DEFAULT 0 CHECK(kind_code BETWEEN 0 AND 2);
UPDATE objects SET kind_code=CASE old_kind WHEN 'coll' THEN 1 WHEN 'marriage' THEN 2 ELSE 0 END
    WHERE old_kind<>'file';
ALTER TABLE objects DROP COLUMN old_kind;
ALTER TABLE objects ADD COLUMN kind TEXT GENERATED ALWAYS AS
    (CASE kind_code WHEN 0 THEN 'file' WHEN 1 THEN 'coll' WHEN 2 THEN 'marriage' END) VIRTUAL;

ALTER TABLE objects DROP COLUMN extension;
ALTER TABLE objects ADD COLUMN extension TEXT GENERATED ALWAYS AS
    (CASE WHEN path_folder IS NOT NULL OR path_literal IS NOT NULL
          THEN mosh_file_extension(name,kind_code) END) VIRTUAL;
ALTER TABLE objects DROP COLUMN allocated;
ALTER TABLE objects DROP COLUMN accessed;
ALTER TABLE objects DROP COLUMN attributes;
ALTER TABLE objects DROP COLUMN volume;
ALTER TABLE objects DROP COLUMN filesystem;
ALTER TABLE objects DROP COLUMN hardlinks;
ALTER TABLE objects ADD COLUMN accessed INTEGER GENERATED ALWAYS AS (NULL) VIRTUAL;

CREATE INDEX live_path_key ON objects(mosh_path_key(path))
    WHERE missing=0 AND path IS NOT NULL;
CREATE INDEX object_secret ON objects(path) WHERE secret=1 AND missing=0;
CREATE INDEX object_size ON objects(size,missing) WHERE kind='file';

CREATE TRIGGER unique_live_path_insert BEFORE INSERT ON objects
WHEN NEW.missing=0 AND NEW.path IS NOT NULL AND EXISTS(
    SELECT 1 FROM objects WHERE missing=0 AND path IS NOT NULL
        AND mosh_path_key(path)=mosh_path_key(NEW.path) AND path=NEW.path AND id<>NEW.id)
BEGIN SELECT RAISE(ABORT,'duplicate live path'); END;
CREATE TRIGGER unique_live_path_update BEFORE UPDATE OF path_literal,path_folder,name,missing ON objects
WHEN NEW.missing=0 AND NEW.path IS NOT NULL AND (OLD.missing<>0 OR OLD.path IS NOT NEW.path)
    AND EXISTS(SELECT 1 FROM objects WHERE missing=0 AND path IS NOT NULL
        AND mosh_path_key(path)=mosh_path_key(NEW.path) AND path=NEW.path AND id<>OLD.id)
BEGIN SELECT RAISE(ABORT,'duplicate live path'); END;

CREATE TRIGGER ledger_insert AFTER INSERT ON objects BEGIN
    INSERT INTO object_keys(object) VALUES(NEW.id);
    INSERT INTO ledger(rowid,name,path,paint)
    SELECT key,NEW.name,coalesce(NEW.path,''),'' FROM object_keys WHERE object=NEW.id AND NEW.missing=0;
END;
CREATE TRIGGER ledger_update AFTER UPDATE OF path_literal,path_folder,name,missing ON objects
WHEN OLD.path IS NOT NEW.path OR OLD.name<>NEW.name OR OLD.missing<>NEW.missing BEGIN
    DELETE FROM ledger WHERE rowid=(SELECT key FROM object_keys WHERE object=OLD.id);
    INSERT INTO ledger(rowid,name,path,paint)
    SELECT key,NEW.name,coalesce(NEW.path,''),coalesce((SELECT body FROM paint WHERE object=NEW.id),'')
    FROM object_keys WHERE object=NEW.id AND NEW.missing=0;
END;
CREATE TRIGGER text_invalidate AFTER UPDATE OF revision,path_literal,path_folder,name,missing,reparse ON objects
WHEN OLD.revision<>NEW.revision OR OLD.path IS NOT NEW.path OR OLD.missing<>NEW.missing OR OLD.reparse<>NEW.reparse BEGIN
    DELETE FROM text_terms WHERE rowid IN(SELECT c.key FROM text_cached c JOIN object_keys k ON k.key=c.key WHERE k.object=NEW.id);
    DELETE FROM text_cached WHERE key=(SELECT key FROM object_keys WHERE object=NEW.id);
END;
CREATE TRIGGER paint_insert AFTER INSERT ON paint BEGIN
    UPDATE ledger SET (name,path,paint)=(SELECT name,coalesce(path,''),NEW.body FROM objects WHERE id=NEW.object)
    WHERE rowid=(SELECT key FROM object_keys WHERE object=NEW.object);
END;
CREATE TRIGGER paint_update AFTER UPDATE OF body ON paint WHEN NEW.body<>OLD.body BEGIN
    UPDATE ledger SET (name,path,paint)=(SELECT name,coalesce(path,''),NEW.body FROM objects WHERE id=NEW.object)
    WHERE rowid=(SELECT key FROM object_keys WHERE object=NEW.object);
END;
CREATE TRIGGER paint_delete AFTER DELETE ON paint BEGIN
    UPDATE ledger SET (name,path,paint)=(SELECT name,coalesce(path,''),'' FROM objects WHERE id=OLD.object)
    WHERE rowid=(SELECT key FROM object_keys WHERE object=OLD.object);
END;
