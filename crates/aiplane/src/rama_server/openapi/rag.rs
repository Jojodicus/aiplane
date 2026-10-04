// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Knowledge bases (RAG collections, sources, extraction profiles).

use super::*;
use crate::rama_server::rag_api as rag;

pub(super) fn operations() -> Vec<Op> {
    vec![
        get("/api/v0/rag/providers", Access::Admin).ok(json::<rag::ProvidersView>()),
        post("/api/v0/rag/test-source", Access::Admin)
            .body(json::<rag::TestSourceRequest>())
            .ok(json::<rag::SourceProbe>())
            .errors(&[400, 502]),
        get("/api/v0/rag/profiles", Access::Admin).ok(json::<rag::ProfileList>()),
        post("/api/v0/rag/profiles", Access::Admin)
            .body(json::<rag::ProfileRequest>())
            .ok(json::<rag::ProfileSaved>())
            .errors(&[400, 409]),
        put("/api/v0/rag/profiles/{name}", Access::Admin)
            .body(json::<rag::ProfileRequest>())
            .ok(json::<rag::ProfileUpdated>())
            .errors(&[400, 404]),
        delete("/api/v0/rag/profiles/{name}", Access::Admin)
            .ok(json::<rag::Deleted>())
            .errors(&[400, 404, 409]),
        get("/api/v0/rag/collections", Access::Admin).ok(json::<rag::CollectionList>()),
        post("/api/v0/rag/collections", Access::Admin)
            .body(json::<rag::CreateRequest>())
            .created(json::<rag::CollectionView>())
            .errors(&[400, 409]),
        get("/api/v0/rag/collections/{id}", Access::Admin)
            .ok(json::<rag::CollectionView>())
            .errors(&[404]),
        patch("/api/v0/rag/collections/{id}", Access::Admin)
            .body(json::<rag::UpdateRequest>())
            .ok(json::<rag::CollectionView>())
            .errors(&[400, 404]),
        delete("/api/v0/rag/collections/{id}", Access::Admin)
            .ok(json::<rag::Deleted>())
            .errors(&[404]),
        post("/api/v0/rag/collections/{id}/reindex", Access::Admin)
            .ok(json::<rag::CollectionView>())
            .errors(&[404]),
        get("/api/v0/rag/collections/{id}/refs", Access::Admin)
            .ok(json::<rag::RefList>())
            .errors(&[404]),
        post("/api/v0/rag/collections/{id}/refs", Access::Admin)
            .body(json::<rag::AddRefsRequest>())
            .ok(json::<rag::AddedRefs>())
            .errors(&[400, 404]),
        delete("/api/v0/rag/collections/{id}/refs/{ref_id}", Access::Admin)
            .ok(json::<rag::RefDeleted>())
            .errors(&[404]),
        patch("/api/v0/rag/collections/{id}/refs/{ref_id}", Access::Admin)
            .body(json::<rag::UpdateRefRequest>())
            .ok(json::<rag::RefUpdated>())
            .errors(&[400, 404]),
        get(
            "/api/v0/rag/collections/{id}/refs/{ref_id}/log",
            Access::Admin,
        )
        .ok(json::<rag::LogList>())
        .errors(&[404]),
        post(
            "/api/v0/rag/collections/{id}/refs/{ref_id}/rebuild",
            Access::Admin,
        )
        .ok(json::<rag::RebuildRequested>())
        .errors(&[400, 404]),
        post(
            "/api/v0/rag/collections/{id}/refs/{ref_id}/primary",
            Access::Admin,
        )
        .ok(json::<rag::PrimarySet>())
        .errors(&[404]),
        post("/api/v0/rag/collections/{id}/sync-token", Access::Admin)
            .ok(json::<rag::SyncToken>())
            .errors(&[404]),
        post(
            "/api/v0/rag/collections/{id}/sync-token/clear",
            Access::Admin,
        )
        .ok(json::<rag::Cleared>())
        .errors(&[404]),
    ]
}
