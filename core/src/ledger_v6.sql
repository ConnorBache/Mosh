DROP INDEX live_path;
CREATE INDEX live_path_key ON objects(mosh_path_key(path))
    WHERE missing=0 AND path IS NOT NULL;
CREATE TRIGGER unique_live_path_insert BEFORE INSERT ON objects
WHEN NEW.missing=0 AND NEW.path IS NOT NULL AND EXISTS(
    SELECT 1 FROM objects WHERE missing=0 AND path IS NOT NULL
        AND mosh_path_key(path)=mosh_path_key(NEW.path) AND path=NEW.path AND id<>NEW.id)
BEGIN SELECT RAISE(ABORT,'duplicate live path'); END;
CREATE TRIGGER unique_live_path_update BEFORE UPDATE OF path,missing ON objects
WHEN NEW.missing=0 AND NEW.path IS NOT NULL AND (OLD.missing<>0 OR OLD.path IS NOT NEW.path)
    AND EXISTS(SELECT 1 FROM objects WHERE missing=0 AND path IS NOT NULL
        AND mosh_path_key(path)=mosh_path_key(NEW.path) AND path=NEW.path AND id<>OLD.id)
BEGIN SELECT RAISE(ABORT,'duplicate live path'); END;

DROP INDEX object_git;
CREATE INDEX object_git ON objects(git_root,git_status) WHERE git_root IS NOT NULL;
DROP INDEX file_tier_rank;
DROP INDEX object_parent;
CREATE INDEX object_parent ON objects(parent,missing,name COLLATE NOCASE);
UPDATE file_tiers SET assessor=CASE assessor WHEN 'filesystem' THEN 'f' WHEN 'ai' THEN 'a' ELSE assessor END
    WHERE assessor IN ('filesystem','ai');
