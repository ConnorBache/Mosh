DROP TRIGGER unique_live_path_insert;
DROP TRIGGER unique_live_path_update;
DROP INDEX live_path_key;
DROP INDEX object_parent_key;
DROP INDEX tag_set_owners;

DROP TRIGGER path_folders_immutable;
CREATE TRIGGER path_folders_immutable BEFORE UPDATE OF prefix ON path_folders
BEGIN SELECT RAISE(ABORT,'path directory spellings are immutable'); END;
ALTER TABLE path_folders ADD COLUMN coll_key INTEGER REFERENCES objects(key);
CREATE INDEX object_literal_path ON objects(mosh_path_key(path_literal))
    WHERE missing=0 AND path_literal IS NOT NULL;
UPDATE path_folders AS f SET coll_key=(
    SELECT key FROM objects o WHERE o.missing=0 AND o.kind_code=1 AND o.path_literal IS NOT NULL
      AND mosh_path_key(o.path_literal) IN (mosh_path_key(f.prefix),mosh_path_key(substr(f.prefix,1,length(f.prefix)-1)))
      AND o.path_literal IN (f.prefix,substr(f.prefix,1,length(f.prefix)-1))
      AND mosh_directory_prefix(o.path_literal)=f.prefix
);
CREATE INDEX path_folders_coll ON path_folders(coll_key) WHERE coll_key IS NOT NULL;

ALTER TABLE objects DROP COLUMN parent;
ALTER TABLE objects RENAME COLUMN parent_key TO parent_override;
UPDATE objects SET parent_override=NULL WHERE path_folder IS NOT NULL
    AND parent_override IS (SELECT coll_key FROM path_folders WHERE id=path_folder);
ALTER TABLE objects ADD COLUMN parent_key INTEGER GENERATED ALWAYS AS
    (coalesce(parent_override,mosh_folder_coll(path_folder))) VIRTUAL;
ALTER TABLE objects ADD COLUMN parent TEXT GENERATED ALWAYS AS (mosh_public_id(parent_key)) VIRTUAL;

-- One index serves both directory membership and a short basename fingerprint.
-- Exact spelling is always checked after the fingerprint.
CREATE INDEX object_folder_members ON objects(path_folder,mosh_name_key(name))
    WHERE missing=0 AND path_folder IS NOT NULL;
CREATE INDEX object_parent_override ON objects(parent_override)
    WHERE missing=0 AND parent_override IS NOT NULL;
CREATE VIEW physical_children AS
    SELECT parent_override AS parent,key AS child FROM objects
        WHERE missing=0 AND parent_override IS NOT NULL
    UNION ALL
    SELECT f.coll_key,o.key FROM path_folders f JOIN objects o ON o.path_folder=f.id
        WHERE f.coll_key IS NOT NULL AND o.missing=0 AND o.parent_override IS NULL;

CREATE TRIGGER object_location_insert BEFORE INSERT ON objects
WHEN NEW.missing=0 AND (NEW.path_folder IS NOT NULL OR NEW.path_literal IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT,'duplicate live path') WHERE EXISTS(
        SELECT 1 FROM objects o WHERE o.missing=0 AND o.path_folder=coalesce(NEW.path_folder,
            (SELECT id FROM path_folders WHERE prefix=mosh_path_prefix(NEW.path_literal,mosh_basename(NEW.path_literal))))
            AND mosh_name_key(o.name)=mosh_name_key(CASE WHEN NEW.path_folder IS NOT NULL THEN NEW.name ELSE mosh_basename(NEW.path_literal) END)
            AND o.name=CASE WHEN NEW.path_folder IS NOT NULL THEN NEW.name ELSE mosh_basename(NEW.path_literal) END AND o.key<>NEW.key
        UNION ALL
        SELECT 1 FROM objects o WHERE o.missing=0 AND o.path_literal IS NOT NULL
            AND mosh_path_key(o.path_literal)=mosh_path_key(NEW.path) AND o.path_literal=NEW.path AND o.key<>NEW.key
    );
END;
CREATE TRIGGER object_location_update BEFORE UPDATE OF path_literal,path_folder,name,missing ON objects
WHEN NEW.missing=0 AND (NEW.path_folder IS NOT NULL OR NEW.path_literal IS NOT NULL) AND (OLD.missing<>0 OR OLD.path_folder IS NOT NEW.path_folder OR OLD.path_literal IS NOT NEW.path_literal OR OLD.name<>NEW.name)
BEGIN
    SELECT RAISE(ABORT,'duplicate live path') WHERE EXISTS(
        SELECT 1 FROM objects o WHERE o.missing=0 AND o.path_folder=coalesce(NEW.path_folder,
            (SELECT id FROM path_folders WHERE prefix=mosh_path_prefix(NEW.path_literal,mosh_basename(NEW.path_literal))))
            AND mosh_name_key(o.name)=mosh_name_key(CASE WHEN NEW.path_folder IS NOT NULL THEN NEW.name ELSE mosh_basename(NEW.path_literal) END)
            AND o.name=CASE WHEN NEW.path_folder IS NOT NULL THEN NEW.name ELSE mosh_basename(NEW.path_literal) END AND o.key<>OLD.key
        UNION ALL
        SELECT 1 FROM objects o WHERE o.missing=0 AND o.path_literal IS NOT NULL
            AND mosh_path_key(o.path_literal)=mosh_path_key(NEW.path) AND o.path_literal=NEW.path AND o.key<>OLD.key
    );
END;
CREATE TRIGGER coll_folder_insert AFTER INSERT ON objects
WHEN NEW.kind_code=1 AND NEW.missing=0 AND NEW.path_literal IS NOT NULL
BEGIN
    UPDATE path_folders SET coll_key=NEW.key WHERE prefix=mosh_directory_prefix(NEW.path_literal);
END;
CREATE TRIGGER coll_folder_update AFTER UPDATE OF path_literal,missing ON objects
WHEN NEW.kind_code=1 AND (OLD.missing<>NEW.missing OR OLD.path_literal IS NOT NEW.path_literal)
BEGIN
    UPDATE path_folders SET coll_key=NULL WHERE coll_key=OLD.key;
    UPDATE path_folders SET coll_key=NEW.key WHERE NEW.missing=0 AND prefix=mosh_directory_prefix(NEW.path_literal);
END;

ALTER TABLE tag_sets ADD COLUMN owners INTEGER NOT NULL DEFAULT 0 CHECK(owners>=0);
UPDATE tag_sets SET owners=counts.n
FROM (SELECT tag_set,count(*) n FROM objects WHERE tag_set IS NOT NULL GROUP BY tag_set) counts
WHERE counts.tag_set=tag_sets.id;
DELETE FROM tag_sets WHERE owners=0;
CREATE TRIGGER tag_set_owner_insert AFTER INSERT ON objects WHEN NEW.tag_set IS NOT NULL BEGIN
    UPDATE tag_sets SET owners=owners+1 WHERE id=NEW.tag_set;
END;
CREATE TRIGGER tag_set_owner_update AFTER UPDATE OF tag_set ON objects WHEN OLD.tag_set IS NOT NEW.tag_set BEGIN
    UPDATE tag_sets SET owners=owners+1 WHERE id=NEW.tag_set;
    UPDATE tag_sets SET owners=owners-1 WHERE id=OLD.tag_set;
    DELETE FROM tag_sets WHERE id=OLD.tag_set AND owners=0;
END;
CREATE TRIGGER tag_set_owner_delete AFTER DELETE ON objects WHEN OLD.tag_set IS NOT NULL BEGIN
    UPDATE tag_sets SET owners=owners-1 WHERE id=OLD.tag_set;
    DELETE FROM tag_sets WHERE id=OLD.tag_set AND owners=0;
END;
