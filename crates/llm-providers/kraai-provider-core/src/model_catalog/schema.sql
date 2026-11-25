CREATE TABLE meta (
    id INTEGER PRIMARY KEY CHECK (id = 0),
    generation INTEGER NOT NULL,
    fetched_at INTEGER NOT NULL
);

CREATE TABLE providers (
    id TEXT PRIMARY KEY NOT NULL,
    api TEXT,
    folded_name TEXT
) WITHOUT ROWID;

CREATE INDEX providers_by_api ON providers (api);
CREATE INDEX providers_by_folded_name ON providers (folded_name);

CREATE TABLE models (
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    canonical_model_id TEXT,
    name TEXT,
    max_context INTEGER,
    max_output INTEGER,
    supports_images INTEGER,
    options TEXT,
    priced INTEGER NOT NULL,
    unknown_cost_dimensions INTEGER NOT NULL,
    input INTEGER,
    output INTEGER,
    cache_read INTEGER,
    cache_write INTEGER,
    reasoning INTEGER,
    PRIMARY KEY (provider, model)
) WITHOUT ROWID;

CREATE INDEX models_by_model ON models (model);

CREATE TABLE cost_tiers (
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    position INTEGER NOT NULL,
    size INTEGER NOT NULL,
    input INTEGER,
    output INTEGER,
    cache_read INTEGER,
    cache_write INTEGER,
    reasoning INTEGER,
    PRIMARY KEY (provider, model, position)
) WITHOUT ROWID;
