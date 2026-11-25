DROP TRIGGER ledger_insert;
DROP TRIGGER ledger_update;
DROP TRIGGER paint_insert;
DROP TRIGGER paint_update;
DROP TRIGGER paint_delete;
DROP TABLE ledger;

CREATE VIRTUAL TABLE ledger USING fts5(name,paint,content='',contentless_delete=1,tokenize='unicode61');
CREATE VIRTUAL TABLE directory_fts USING fts5(relative,content='',contentless_delete=1,tokenize='unicode61');

CREATE VIEW path_folder_search AS
    SELECT f.id,f.prefix,(SELECT mosh_search_relative(f.prefix,r.path) FROM roots r
        WHERE mosh_search_relative(f.prefix,r.path) IS NOT NULL
        ORDER BY length(r.path) DESC,r.path LIMIT 1) relative
    FROM path_folders f WHERE NOT EXISTS(SELECT 1 FROM nested_moshes b
        WHERE mosh_search_relative(f.prefix,b.root) IS NOT NULL);

CREATE TRIGGER path_search_directory_insert AFTER INSERT ON path_folders BEGIN
    INSERT INTO directory_fts(rowid,relative)
    SELECT id,relative FROM path_folder_search WHERE id=NEW.id AND relative<>'';
END;
CREATE TRIGGER path_search_directory_delete AFTER DELETE ON path_folders BEGIN
    DELETE FROM directory_fts WHERE rowid=OLD.id;
END;
CREATE TRIGGER path_search_root_insert AFTER INSERT ON roots BEGIN
    DELETE FROM directory_fts WHERE rowid IN
        (SELECT id FROM path_folders WHERE mosh_search_relative(prefix,NEW.path) IS NOT NULL);
    INSERT INTO directory_fts(rowid,relative) SELECT id,relative FROM path_folder_search
    WHERE mosh_search_relative(prefix,NEW.path) IS NOT NULL AND relative<>'';
END;
CREATE TRIGGER path_search_root_delete AFTER DELETE ON roots BEGIN
    DELETE FROM directory_fts WHERE rowid IN
        (SELECT id FROM path_folders WHERE mosh_search_relative(prefix,OLD.path) IS NOT NULL);
    INSERT INTO directory_fts(rowid,relative) SELECT id,relative FROM path_folder_search
    WHERE mosh_search_relative(prefix,OLD.path) IS NOT NULL AND relative<>'';
END;

CREATE TRIGGER ledger_insert AFTER INSERT ON objects BEGIN
    INSERT INTO object_keys(object) VALUES(NEW.id);
    INSERT INTO ledger(rowid,name,paint)
    SELECT key,NEW.name,'' FROM object_keys WHERE object=NEW.id AND NEW.missing=0;
END;
CREATE TRIGGER ledger_update AFTER UPDATE OF name,missing ON objects
WHEN OLD.name<>NEW.name OR OLD.missing<>NEW.missing BEGIN
    DELETE FROM ledger WHERE rowid=(SELECT key FROM object_keys WHERE object=OLD.id);
    INSERT INTO ledger(rowid,name,paint)
    SELECT key,NEW.name,coalesce((SELECT body FROM paint WHERE object=NEW.id),'')
    FROM object_keys WHERE object=NEW.id AND NEW.missing=0;
END;
CREATE TRIGGER paint_insert AFTER INSERT ON paint BEGIN
    UPDATE ledger SET (name,paint)=(SELECT name,NEW.body FROM objects WHERE id=NEW.object)
    WHERE rowid=(SELECT key FROM object_keys WHERE object=NEW.object);
END;
CREATE TRIGGER paint_update AFTER UPDATE OF body ON paint WHEN NEW.body<>OLD.body BEGIN
    UPDATE ledger SET (name,paint)=(SELECT name,NEW.body FROM objects WHERE id=NEW.object)
    WHERE rowid=(SELECT key FROM object_keys WHERE object=NEW.object);
END;
CREATE TRIGGER paint_delete AFTER DELETE ON paint BEGIN
    UPDATE ledger SET (name,paint)=(SELECT name,'' FROM objects WHERE id=OLD.object)
    WHERE rowid=(SELECT key FROM object_keys WHERE object=OLD.object);
END;
