// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Virtual channel management endpoints.

use std::sync::Arc;

use actix_web::{delete, get, post, put, web, HttpResponse, Responder};
use actix_web::Error;
use tokio::sync::mpsc;
use uuid::Uuid;

use trx_core::radio::freq::Freq;
use trx_core::{RigCommand, RigRequest};
use trx_protocol::parse_mode;

use crate::server::vchan::ClientChannelManager;

use super::send_command_to_rig;

// ============================================================================
// Channel CRUD
// ============================================================================

#[get("/channels/{remote}")]
pub async fn list_channels(
    path: web::Path<String>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
) -> impl Responder {
    let remote = path.into_inner();
    HttpResponse::Ok().json(vchan_mgr.channels(&remote))
}

#[derive(serde::Deserialize)]
struct AllocateChannelBody {
    session_id: Uuid,
    freq_hz: u64,
    mode: String,
}

#[post("/channels/{remote}")]
pub async fn allocate_channel(
    path: web::Path<String>,
    body: web::Json<AllocateChannelBody>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
) -> impl Responder {
    let remote = path.into_inner();
    match vchan_mgr.allocate(body.session_id, &remote, body.freq_hz, &body.mode) {
        Ok(ch) => HttpResponse::Ok().json(ch),
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}

#[delete("/channels/{remote}/{channel_id}")]
pub async fn delete_channel_route(
    path: web::Path<(String, Uuid)>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
) -> impl Responder {
    let (remote, channel_id) = path.into_inner();
    match vchan_mgr.delete_channel(&remote, channel_id) {
        Ok(()) => HttpResponse::Ok().finish(),
        Err(crate::server::vchan::VChanClientError::NotFound) => HttpResponse::NotFound().finish(),
        Err(crate::server::vchan::VChanClientError::Permanent) => {
            HttpResponse::BadRequest().body("cannot remove the primary channel")
        }
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct SubscribeBody {
    session_id: Uuid,
}

#[post("/channels/{remote}/{channel_id}/subscribe")]
pub async fn subscribe_channel(
    path: web::Path<(String, Uuid)>,
    body: web::Json<SubscribeBody>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
    bookmark_store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    scheduler_control: web::Data<crate::server::scheduler::SharedSchedulerControlManager>,
) -> impl Responder {
    let body = body.into_inner();
    let (remote, channel_id) = path.into_inner();
    match vchan_mgr.subscribe_session(body.session_id, &remote, channel_id) {
        Some(ch) => {
            scheduler_control.set_released(body.session_id, false);
            let Some(selected) = vchan_mgr.selected_channel(&remote, channel_id) else {
                return HttpResponse::InternalServerError().body("subscribed channel missing");
            };
            if let Err(err) = apply_selected_channel(
                rig_tx.get_ref(),
                &remote,
                &selected,
                bookmark_store_map.get_ref().as_ref(),
            )
            .await
            {
                return HttpResponse::from_error(err);
            }
            HttpResponse::Ok().json(ch)
        }
        None => HttpResponse::NotFound().finish(),
    }
}

// ============================================================================
// Channel property updates
// ============================================================================

#[derive(serde::Deserialize)]
struct SetChanFreqBody {
    freq_hz: u64,
}

#[put("/channels/{remote}/{channel_id}/freq")]
pub async fn set_vchan_freq(
    path: web::Path<(String, Uuid)>,
    body: web::Json<SetChanFreqBody>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
) -> impl Responder {
    let (remote, channel_id) = path.into_inner();
    match vchan_mgr.set_channel_freq(&remote, channel_id, body.freq_hz) {
        Ok(()) => HttpResponse::Ok().finish(),
        Err(crate::server::vchan::VChanClientError::NotFound) => HttpResponse::NotFound().finish(),
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct SetChanBwBody {
    bandwidth_hz: u32,
}

#[put("/channels/{remote}/{channel_id}/bw")]
pub async fn set_vchan_bw(
    path: web::Path<(String, Uuid)>,
    body: web::Json<SetChanBwBody>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
) -> impl Responder {
    let (remote, channel_id) = path.into_inner();
    match vchan_mgr.set_channel_bandwidth(&remote, channel_id, body.bandwidth_hz) {
        Ok(()) => HttpResponse::Ok().finish(),
        Err(crate::server::vchan::VChanClientError::NotFound) => HttpResponse::NotFound().finish(),
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct SetChanModeBody {
    mode: String,
}

#[put("/channels/{remote}/{channel_id}/mode")]
pub async fn set_vchan_mode(
    path: web::Path<(String, Uuid)>,
    body: web::Json<SetChanModeBody>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
) -> impl Responder {
    let (remote, channel_id) = path.into_inner();
    match vchan_mgr.set_channel_mode(&remote, channel_id, &body.mode) {
        Ok(()) => HttpResponse::Ok().finish(),
        Err(crate::server::vchan::VChanClientError::NotFound) => HttpResponse::NotFound().finish(),
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn bookmark_decoder_state(
    bookmark: &crate::server::bookmarks::Bookmark,
) -> (bool, bool, bool, bool, bool, bool, bool) {
    let mut want_aprs = bookmark.mode.trim().eq_ignore_ascii_case("PKT");
    let mut want_hf_aprs = false;
    let mut want_ft8 = false;
    let mut want_ft4 = false;
    let mut want_ft2 = false;
    let mut want_wspr = false;
    let mut want_lrpt = false;

    for decoder in bookmark
        .decoders
        .iter()
        .map(|item| item.trim().to_ascii_lowercase())
    {
        match decoder.as_str() {
            "aprs" => want_aprs = true,
            "hf-aprs" => want_hf_aprs = true,
            "ft8" => want_ft8 = true,
            "ft4" => want_ft4 = true,
            "ft2" => want_ft2 = true,
            "wspr" => want_wspr = true,
            "lrpt" => want_lrpt = true,
            _ => {}
        }
    }

    (
        want_aprs,
        want_hf_aprs,
        want_ft8,
        want_ft4,
        want_ft2,
        want_wspr,
        want_lrpt,
    )
}

async fn apply_selected_channel(
    rig_tx: &mpsc::Sender<RigRequest>,
    remote: &str,
    channel: &crate::server::vchan::SelectedChannel,
    bookmark_store_map: &crate::server::bookmarks::BookmarkStoreMap,
) -> Result<(), Error> {
    send_command_to_rig(
        rig_tx,
        remote,
        RigCommand::SetMode(parse_mode(&channel.mode)),
    )
    .await?;

    if channel.bandwidth_hz > 0 {
        send_command_to_rig(
            rig_tx,
            remote,
            RigCommand::SetBandwidth(channel.bandwidth_hz),
        )
        .await?;
    }

    send_command_to_rig(
        rig_tx,
        remote,
        RigCommand::SetFreq(Freq {
            hz: channel.freq_hz,
        }),
    )
    .await?;

    let Some(bookmark_id) = channel.scheduler_bookmark_id.as_deref() else {
        return Ok(());
    };
    let Some(bookmark) = bookmark_store_map.get_for_rig(remote, bookmark_id) else {
        return Ok(());
    };
    let (want_aprs, want_hf_aprs, want_ft8, want_ft4, want_ft2, want_wspr, want_lrpt) =
        bookmark_decoder_state(&bookmark);
    let desired = [
        RigCommand::SetAprsDecodeEnabled(want_aprs),
        RigCommand::SetHfAprsDecodeEnabled(want_hf_aprs),
        RigCommand::SetFt8DecodeEnabled(want_ft8),
        RigCommand::SetFt4DecodeEnabled(want_ft4),
        RigCommand::SetFt2DecodeEnabled(want_ft2),
        RigCommand::SetWsprDecodeEnabled(want_wspr),
        RigCommand::SetLrptDecodeEnabled(want_lrpt),
    ];
    for cmd in desired {
        send_command_to_rig(rig_tx, remote, cmd).await?;
    }

    Ok(())
}
