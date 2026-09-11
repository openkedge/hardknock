-- SPDX-License-Identifier: Apache-2.0
-- Atomic aggregate is the sole source of truth for each explicit hierarchy.
-- V0.17 relation tables remain untouched; adapters project them in memory.
CREATE TABLE knowledge_hierarchies (
    id TEXT PRIMARY KEY CHECK(id LIKE 'hierarchy-%'),
    revision INTEGER NOT NULL CHECK(revision > 0),
    data TEXT NOT NULL CHECK(json_valid(data))
);
