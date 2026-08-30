CREATE TABLE experience_nodes (
 id TEXT PRIMARY KEY,
 name TEXT NOT NULL,
 node_type TEXT NOT NULL,
 public_key TEXT NOT NULL UNIQUE,
 created_at TEXT NOT NULL,
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE federation_peers (
 id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL UNIQUE,
 name TEXT NOT NULL UNIQUE,
 public_key TEXT NOT NULL UNIQUE,
 trust TEXT NOT NULL CHECK(trust IN ('unknown','known','trusted','blocked')),
 added_at TEXT NOT NULL,
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE federation_bundles (
 id TEXT PRIMARY KEY,
 direction TEXT NOT NULL CHECK(direction IN ('published','received')),
 producer TEXT NOT NULL,
 created_at TEXT NOT NULL,
 recorded_at TEXT NOT NULL,
 payload_hash TEXT NOT NULL,
 authenticity TEXT NOT NULL,
 path TEXT,
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX federation_bundle_lookup ON federation_bundles(direction,producer,created_at,id);
CREATE TABLE federated_objects (
 id TEXT PRIMARY KEY,
 origin_node TEXT NOT NULL,
 origin_object_id TEXT NOT NULL,
 origin_bundle TEXT NOT NULL REFERENCES federation_bundles(id),
 object_type TEXT NOT NULL,
 lineage_hash TEXT NOT NULL,
 state TEXT NOT NULL,
 context_score REAL NOT NULL CHECK(context_score >= 0.0 AND context_score <= 1.0),
 created_at TEXT NOT NULL,
 data TEXT NOT NULL CHECK(json_valid(data)),
 UNIQUE(origin_node,origin_object_id,lineage_hash)
);
CREATE INDEX federated_object_search ON federated_objects(object_type,state,context_score DESC,created_at DESC);
CREATE INDEX federated_lineage ON federated_objects(lineage_hash);
CREATE TABLE federated_reproductions (
 id TEXT PRIMARY KEY,
 object_id TEXT NOT NULL REFERENCES federated_objects(id),
 experiment_id TEXT,
 result TEXT NOT NULL,
 created_at TEXT NOT NULL,
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE federated_conflicts (
 id TEXT PRIMARY KEY,
 object_id TEXT NOT NULL REFERENCES federated_objects(id),
 conflict_type TEXT NOT NULL,
 status TEXT NOT NULL,
 created_at TEXT NOT NULL,
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE federation_provenance_nodes (
 id TEXT PRIMARY KEY,
 kind TEXT NOT NULL,
 lineage_hash TEXT,
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE federation_provenance_edges (
 source TEXT NOT NULL REFERENCES federation_provenance_nodes(id),
 target TEXT NOT NULL REFERENCES federation_provenance_nodes(id),
 relationship TEXT NOT NULL,
 data TEXT NOT NULL CHECK(json_valid(data)),
 PRIMARY KEY(source,target,relationship)
);
CREATE INDEX federation_provenance_lineage ON federation_provenance_nodes(lineage_hash);
CREATE TABLE federation_audit (
 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 event TEXT NOT NULL,
 created_at TEXT NOT NULL,
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE federation_revocations (
 bundle_id TEXT NOT NULL,
 signer TEXT NOT NULL,
 created_at TEXT NOT NULL,
 data TEXT NOT NULL CHECK(json_valid(data)),
 PRIMARY KEY(bundle_id,signer)
);
CREATE TABLE federation_benchmark_runs (
 id TEXT PRIMARY KEY,
 created_at TEXT NOT NULL,
 status TEXT NOT NULL,
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TRIGGER federation_bundle_no_update BEFORE UPDATE ON federation_bundles BEGIN SELECT RAISE(ABORT,'Federated bundles are immutable'); END;
CREATE TRIGGER federation_bundle_no_delete BEFORE DELETE ON federation_bundles BEGIN SELECT RAISE(ABORT,'Federated bundles are retained'); END;
CREATE TRIGGER federation_reproduction_no_update BEFORE UPDATE ON federated_reproductions BEGIN SELECT RAISE(ABORT,'Reproduction evidence is immutable'); END;
CREATE TRIGGER federation_reproduction_no_delete BEFORE DELETE ON federated_reproductions BEGIN SELECT RAISE(ABORT,'Reproduction evidence is retained'); END;
CREATE TRIGGER federation_audit_no_update BEFORE UPDATE ON federation_audit BEGIN SELECT RAISE(ABORT,'Federation audit is append only'); END;
CREATE TRIGGER federation_audit_no_delete BEFORE DELETE ON federation_audit BEGIN SELECT RAISE(ABORT,'Federation audit is append only'); END;
CREATE TRIGGER federation_provenance_node_no_update BEFORE UPDATE ON federation_provenance_nodes BEGIN SELECT RAISE(ABORT,'Provenance nodes are immutable'); END;
CREATE TRIGGER federation_provenance_node_no_delete BEFORE DELETE ON federation_provenance_nodes BEGIN SELECT RAISE(ABORT,'Provenance nodes are retained'); END;
CREATE TRIGGER federation_provenance_edge_no_update BEFORE UPDATE ON federation_provenance_edges BEGIN SELECT RAISE(ABORT,'Provenance edges are immutable'); END;
CREATE TRIGGER federation_provenance_edge_no_delete BEFORE DELETE ON federation_provenance_edges BEGIN SELECT RAISE(ABORT,'Provenance edges are retained'); END;
