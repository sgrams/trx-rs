// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Bookmark CRUD endpoints.

use std::sync::Arc;

use actix_web::{delete, get, post, put, web, HttpRequest, HttpResponse};
use actix_web::Error;

use super::{no_cache_response, request_accepts_html, require_control};
use crate::server::status;

// ============================================================================
// Types
// ============================================================================

#[derive(serde::Deserialize)]
pub struct BookmarkQuery {
    pub category: Option<String>,
    pub scope: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct BookmarkScopeQuery {
    pub scope: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct BookmarkInput {
    pub name: String,
    pub freq_hz: u64,
    pub mode: String,
    pub bandwidth_hz: Option<u64>,
    pub locator: Option<String>,
    pub comment: Option<String>,
    pub category: Option<String>,
    pub decoders: Option<Vec<String>>,
}

/// A bookmark with its owning scope tag for the list response.
#[derive(serde::Serialize)]
struct BookmarkWithScope {
    #[serde(flatten)]
    bm: crate::server::bookmarks::Bookmark,
    scope: String,
}

#[derive(serde::Deserialize)]
struct BatchDeleteRequest {
    ids: Vec<String>,
}

#[derive(serde::Deserialize)]
struct BatchMoveRequest {
    ids: Vec<String>,
    to: String,
}

// ============================================================================
// Helpers
// ============================================================================

/// Resolve which `BookmarkStore` to use based on the `scope` parameter.
fn resolve_bookmark_store(
    scope: Option<&str>,
    store_map: &crate::server::bookmarks::BookmarkStoreMap,
) -> std::sync::Arc<crate::server::bookmarks::BookmarkStore> {
    match scope.filter(|s| !s.is_empty() && *s != "general") {
        Some(remote) => store_map.store_for(remote),
        None => store_map.general().clone(),
    }
}

fn gen_bookmark_id() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

fn normalize_bookmark_locator(locator: Option<String>) -> Option<String> {
    locator.and_then(|value| {
        let trimmed = value.trim().to_uppercase();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

// ============================================================================
// Endpoints
// ============================================================================

#[get("/bookmarks")]
pub async fn list_bookmarks(
    req: HttpRequest,
    store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    query: web::Query<BookmarkQuery>,
) -> Result<HttpResponse, Error> {
    if request_accepts_html(&req) {
        return Ok(no_cache_response(
            "text/html; charset=utf-8",
            status::index_html(),
        ));
    }
    let scope = query
        .scope
        .as_deref()
        .filter(|s| !s.is_empty() && *s != "general");
    let mut list: Vec<BookmarkWithScope> = match scope {
        Some(remote) => {
            let mut map: std::collections::HashMap<String, BookmarkWithScope> = store_map
                .general()
                .list()
                .into_iter()
                .map(|bm| {
                    let id = bm.id.clone();
                    (
                        id,
                        BookmarkWithScope {
                            bm,
                            scope: "general".into(),
                        },
                    )
                })
                .collect();
            for bm in store_map.store_for(remote).list() {
                let id = bm.id.clone();
                map.insert(
                    id,
                    BookmarkWithScope {
                        bm,
                        scope: remote.to_owned(),
                    },
                );
            }
            map.into_values().collect()
        }
        None => store_map
            .general()
            .list()
            .into_iter()
            .map(|bm| BookmarkWithScope {
                bm,
                scope: "general".into(),
            })
            .collect(),
    };
    if let Some(ref cat) = query.category {
        if !cat.is_empty() {
            let cat_lower = cat.to_lowercase();
            list.retain(|item| item.bm.category.to_lowercase() == cat_lower);
        }
    }
    list.sort_by_key(|item| item.bm.freq_hz);
    Ok(HttpResponse::Ok().json(list))
}

#[post("/bookmarks")]
pub async fn create_bookmark(
    req: HttpRequest,
    store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    query: web::Query<BookmarkScopeQuery>,
    body: web::Json<BookmarkInput>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    let store = resolve_bookmark_store(query.scope.as_deref(), store_map.get_ref());
    if store.freq_taken(body.freq_hz, None) {
        return Err(actix_web::error::ErrorConflict(
            "a bookmark for that frequency already exists",
        ));
    }
    let bm = crate::server::bookmarks::Bookmark {
        id: gen_bookmark_id(),
        name: body.name.clone(),
        freq_hz: body.freq_hz,
        mode: body.mode.clone(),
        bandwidth_hz: body.bandwidth_hz,
        locator: normalize_bookmark_locator(body.locator.clone()),
        comment: body.comment.clone().unwrap_or_default(),
        category: body.category.clone().unwrap_or_default(),
        decoders: body.decoders.clone().unwrap_or_default(),
    };
    if store.insert(&bm) {
        Ok(HttpResponse::Created().json(bm))
    } else {
        Err(actix_web::error::ErrorInternalServerError(
            "failed to save bookmark",
        ))
    }
}

#[put("/bookmarks/{id}")]
pub async fn update_bookmark(
    req: HttpRequest,
    path: web::Path<String>,
    store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    query: web::Query<BookmarkScopeQuery>,
    body: web::Json<BookmarkInput>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    let store = resolve_bookmark_store(query.scope.as_deref(), store_map.get_ref());
    let id = path.into_inner();
    if store.freq_taken(body.freq_hz, Some(&id)) {
        return Err(actix_web::error::ErrorConflict(
            "a bookmark for that frequency already exists",
        ));
    }
    let bm = crate::server::bookmarks::Bookmark {
        id: id.clone(),
        name: body.name.clone(),
        freq_hz: body.freq_hz,
        mode: body.mode.clone(),
        bandwidth_hz: body.bandwidth_hz,
        locator: normalize_bookmark_locator(body.locator.clone()),
        comment: body.comment.clone().unwrap_or_default(),
        category: body.category.clone().unwrap_or_default(),
        decoders: body.decoders.clone().unwrap_or_default(),
    };
    if store.upsert(&id, &bm) {
        Ok(HttpResponse::Ok().json(bm))
    } else {
        Err(actix_web::error::ErrorNotFound("bookmark not found"))
    }
}

#[delete("/bookmarks/{id}")]
pub async fn delete_bookmark(
    req: HttpRequest,
    path: web::Path<String>,
    store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    query: web::Query<BookmarkScopeQuery>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    let store = resolve_bookmark_store(query.scope.as_deref(), store_map.get_ref());
    let id = path.into_inner();
    if store.remove(&id) {
        Ok(HttpResponse::Ok().json(serde_json::json!({ "deleted": true })))
    } else {
        Err(actix_web::error::ErrorNotFound("bookmark not found"))
    }
}

#[post("/bookmarks/batch_delete")]
pub async fn batch_delete_bookmarks(
    req: HttpRequest,
    body: web::Json<BatchDeleteRequest>,
    store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    query: web::Query<BookmarkScopeQuery>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    let store = resolve_bookmark_store(query.scope.as_deref(), store_map.get_ref());
    let mut deleted = 0usize;
    for id in &body.ids {
        if store.remove(id) {
            deleted += 1;
        }
    }
    Ok(HttpResponse::Ok().json(serde_json::json!({ "deleted": deleted })))
}

#[post("/bookmarks/batch_move")]
pub async fn batch_move_bookmarks(
    req: HttpRequest,
    body: web::Json<BatchMoveRequest>,
    store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    query: web::Query<BookmarkScopeQuery>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    let from_store = resolve_bookmark_store(query.scope.as_deref(), store_map.get_ref());
    let to_store = resolve_bookmark_store(Some(body.to.as_str()), store_map.get_ref());
    let mut moved = 0usize;
    for id in &body.ids {
        if let Some(bm) = from_store.get(id) {
            if to_store.insert(&bm) && from_store.remove(id) {
                moved += 1;
            }
        }
    }
    Ok(HttpResponse::Ok().json(serde_json::json!({ "moved": moved })))
}
