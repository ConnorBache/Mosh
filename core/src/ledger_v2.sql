DROP TRIGGER ledger_update;
DROP TRIGGER ledger_insert;
DROP TRIGGER paint_insert;
DROP TRIGGER paint_update;
DROP TRIGGER paint_delete;
DROP TABLE ledger;
ALTER TABLE objects DROP COLUMN content;

CREATE VIRTUAL TABLE ledger USING fts5(name,path,paint,content='',contentless_delete=1,tokenize='unicode61');
CREATE VIRTUAL TABLE text_terms USING fts5(body,content='',contentless_delete=1,tokenize='unicode61');
CREATE TABLE text_cached(key INTEGER PRIMARY KEY REFERENCES object_keys(key));

INSERT INTO ledger(rowid,name,path,paint)
    SELECT k.key,o.name,coalesce(o.path,''),coalesce(p.body,'')
    FROM objects o JOIN object_keys k ON k.object=o.id LEFT JOIN paint p ON p.object=o.id WHERE o.missing=0;

CREATE TRIGGER ledger_insert AFTER INSERT ON objects BEGIN
    INSERT INTO object_keys(object) VALUES(NEW.id);
    INSERT INTO ledger(rowid,name,path,paint)
    SELECT key,NEW.name,coalesce(NEW.path,''),'' FROM object_keys WHERE object=NEW.id AND NEW.missing=0;
END;
CREATE TRIGGER ledger_update AFTER UPDATE OF path,name,missing ON objects
WHEN OLD.path IS NOT NEW.path OR OLD.name<>NEW.name OR OLD.missing<>NEW.missing BEGIN
    DELETE FROM ledger WHERE rowid=(SELECT key FROM object_keys WHERE object=OLD.id);
    INSERT INTO ledger(rowid,name,path,paint)
    SELECT key,NEW.name,coalesce(NEW.path,''),coalesce((SELECT body FROM paint WHERE object=NEW.id),'')
    FROM object_keys WHERE object=NEW.id AND NEW.missing=0;
END;
CREATE TRIGGER text_invalidate AFTER UPDATE OF revision,path,missing,reparse ON objects
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
