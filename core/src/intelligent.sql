CREATE TABLE tiers(
    level INTEGER PRIMARY KEY CHECK(level BETWEEN 1 AND 10),
    minimum_tags INTEGER NOT NULL CHECK(minimum_tags>=1),
    minimum_score INTEGER NOT NULL CHECK(minimum_score BETWEEN 0 AND 100),
    passes INTEGER NOT NULL CHECK(passes BETWEEN 1 AND 8),
    paint TEXT CHECK(paint IN ('minimal','standard','detailed')),
    embed INTEGER NOT NULL CHECK(embed IN (0,1))
);
INSERT INTO tiers VALUES(1,1,0,1,NULL,0),(2,5,28,1,NULL,0),
    (3,10,52,2,'minimal',1),(4,100,75,3,'standard',1);
CREATE TABLE file_tiers(
    object_key INTEGER PRIMARY KEY REFERENCES object_keys(key),
    revision INTEGER NOT NULL,
    importance INTEGER NOT NULL CHECK(importance BETWEEN 0 AND 100),
    information INTEGER NOT NULL CHECK(information BETWEEN 0 AND 100),
    predicted_hotness INTEGER NOT NULL CHECK(predicted_hotness BETWEEN 0 AND 100),
    human_accesses INTEGER NOT NULL DEFAULT 0,
    last_access INTEGER,
    target INTEGER NOT NULL DEFAULT 1,
    effective INTEGER,
    requested INTEGER,
    locked INTEGER NOT NULL DEFAULT 0,
    assessor TEXT NOT NULL,
    assessment TEXT,
    assessed INTEGER NOT NULL
);
CREATE INDEX file_tier_rank ON file_tiers(effective,object_key);
CREATE TRIGGER refresh_tag_minimum AFTER UPDATE OF tag_set ON object_keys BEGIN
    UPDATE file_tiers SET effective=(SELECT max(level) FROM tiers WHERE level<=file_tiers.target
        AND minimum_tags<=coalesce((SELECT confirmed FROM tag_sets WHERE id=NEW.tag_set),0))
        WHERE object_key=NEW.key;
END;

CREATE TABLE tag_registry(
    id INTEGER PRIMARY KEY,
    canonical TEXT NOT NULL,
    category TEXT,
    description TEXT,
    created INTEGER,
    creator TEXT,
    modified INTEGER,
    retired INTEGER,
    replacement INTEGER REFERENCES tag_registry(id),
    deleted INTEGER NOT NULL DEFAULT 0
);
INSERT INTO tag_registry(id,canonical) SELECT id,name FROM tag_dictionary;
CREATE TRIGGER registry_new_tag AFTER INSERT ON tag_dictionary BEGIN
    INSERT INTO tag_registry(id,canonical,category,created,modified)
        VALUES(NEW.id,NEW.name,CASE WHEN instr(NEW.name,':')>0 THEN substr(NEW.name,1,instr(NEW.name,':')-1) END,
               CAST(unixepoch('subsec')*1000 AS INTEGER),CAST(unixepoch('subsec')*1000 AS INTEGER));
END;
CREATE TRIGGER registry_rename AFTER UPDATE OF name ON tag_dictionary BEGIN
    UPDATE tag_registry SET canonical=NEW.name,modified=CAST(unixepoch('subsec')*1000 AS INTEGER) WHERE id=NEW.id;
END;
CREATE TRIGGER registry_delete_tag AFTER DELETE ON tag_dictionary BEGIN
    UPDATE tag_registry SET deleted=1,modified=CAST(unixepoch('subsec')*1000 AS INTEGER) WHERE id=OLD.id;
END;
CREATE TABLE tag_history(
    seq INTEGER PRIMARY KEY, tag INTEGER NOT NULL REFERENCES tag_registry(id),
    object_key INTEGER REFERENCES object_keys(key), origin TEXT, action TEXT NOT NULL,
    time INTEGER NOT NULL, actor TEXT NOT NULL, revision INTEGER, details TEXT
);
CREATE INDEX tag_history_tag ON tag_history(tag,seq DESC);
CREATE INDEX tag_history_object ON tag_history(object_key,seq DESC) WHERE object_key IS NOT NULL;
ALTER TABLE tag_details RENAME TO tag_details_v2;
CREATE TABLE tag_details(
    object_key INTEGER NOT NULL REFERENCES object_keys(key), tag INTEGER NOT NULL REFERENCES tag_dictionary(id),
    origin INTEGER NOT NULL CHECK(origin IN (1,2,4,8,16,32,64)),
    confidence REAL NOT NULL CHECK(confidence BETWEEN 0 AND 1), evidence TEXT,
    PRIMARY KEY(object_key,tag,origin)
) WITHOUT ROWID;
INSERT INTO tag_details SELECT * FROM tag_details_v2;
DROP TABLE tag_details_v2;
CREATE TABLE coverage(
    object_key INTEGER NOT NULL REFERENCES object_keys(key), tag INTEGER NOT NULL REFERENCES tag_registry(id),
    revision INTEGER NOT NULL, checked INTEGER NOT NULL, applicable INTEGER NOT NULL,
    PRIMARY KEY(object_key,tag)
) WITHOUT ROWID;

CREATE TABLE ai_jobs(
    id INTEGER PRIMARY KEY, kind TEXT NOT NULL, object_key INTEGER REFERENCES object_keys(key),
    revision INTEGER NOT NULL DEFAULT 0, scope INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL DEFAULT 'queued', priority INTEGER NOT NULL DEFAULT 0, queued INTEGER NOT NULL,
    ready INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0, lease INTEGER, claim TEXT,
    result TEXT, error TEXT,
    UNIQUE(kind,object_key,revision,scope)
);
CREATE INDEX ai_job_pick ON ai_jobs(state,priority DESC,ready,id);
CREATE TABLE ai_usage(
    id INTEGER PRIMARY KEY, operation TEXT NOT NULL, object_key INTEGER REFERENCES object_keys(key),
    provider TEXT NOT NULL, model TEXT NOT NULL, started INTEGER NOT NULL, elapsed INTEGER,
    input INTEGER, output INTEGER, reserved_micros INTEGER NOT NULL, cost_micros INTEGER, outcome TEXT
);
CREATE INDEX ai_usage_time ON ai_usage(started);

CREATE TABLE passages(
    id INTEGER PRIMARY KEY, object_key INTEGER NOT NULL REFERENCES object_keys(key),
    revision INTEGER NOT NULL, first_byte INTEGER NOT NULL, end_byte INTEGER NOT NULL, digest BLOB NOT NULL,
    kind TEXT NOT NULL, actor TEXT, recipient TEXT, topic TEXT, status TEXT, date TEXT, paint TEXT,
    hits INTEGER NOT NULL DEFAULT 0, promoted INTEGER NOT NULL DEFAULT 0,
    UNIQUE(object_key,revision,first_byte,end_byte,kind)
);
CREATE INDEX passage_relation ON passages(kind,actor,recipient,topic,date);
CREATE TABLE passage_tags(
    passage INTEGER NOT NULL REFERENCES passages(id) ON DELETE CASCADE,
    tag INTEGER NOT NULL REFERENCES tag_dictionary(id) ON DELETE CASCADE,
    PRIMARY KEY(passage,tag)
) WITHOUT ROWID;
CREATE INDEX tag_passages ON passage_tags(tag,passage);
CREATE TABLE embeddings(
    object_key INTEGER NOT NULL REFERENCES object_keys(key), passage INTEGER NOT NULL DEFAULT 0,
    model TEXT NOT NULL, revision INTEGER NOT NULL, vector BLOB NOT NULL,
    PRIMARY KEY(object_key,passage,model)
) WITHOUT ROWID;
CREATE INDEX vectors_model ON embeddings(model);

CREATE TABLE retrievals(
    id INTEGER PRIMARY KEY, actor TEXT NOT NULL REFERENCES people(id), request TEXT NOT NULL,
    plan TEXT NOT NULL, result TEXT NOT NULL, metrics TEXT NOT NULL, time INTEGER NOT NULL, replay_of INTEGER
);
CREATE TABLE retrieval_feedback(
    run INTEGER NOT NULL REFERENCES retrievals(id), object_key INTEGER NOT NULL REFERENCES object_keys(key),
    helpful INTEGER NOT NULL, passage INTEGER, time INTEGER NOT NULL,
    PRIMARY KEY(run,object_key)
) WITHOUT ROWID;
CREATE TABLE recommendations(
    id INTEGER PRIMARY KEY, key TEXT NOT NULL UNIQUE, state TEXT NOT NULL DEFAULT 'open',
    message TEXT NOT NULL, details TEXT NOT NULL, time INTEGER NOT NULL
);
CREATE TABLE coll_ai_policy(
    object_key INTEGER PRIMARY KEY REFERENCES object_keys(key),
    mode TEXT, remote INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE coll_security(
    object TEXT PRIMARY KEY REFERENCES objects(id),
    restricted INTEGER NOT NULL CHECK(restricted IN (0,1))
) WITHOUT ROWID;
CREATE TABLE guest_rights(
    object TEXT NOT NULL REFERENCES objects(id),
    person TEXT NOT NULL REFERENCES people(id),
    right TEXT NOT NULL CHECK(right IN ('discover','paint','content','edit','manage')),
    PRIMARY KEY(object,person,right)
) WITHOUT ROWID;
