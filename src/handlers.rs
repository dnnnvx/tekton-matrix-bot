//! Matrix event handlers.

use matrix_sdk::{
    Client, Room, RoomState,
    event_handler::Ctx,
    ruma::events::room::{
        member::StrippedRoomMemberEvent,
        message::{
            AddMentions, ForwardThread, MessageType, OriginalSyncRoomMessageEvent,
            RoomMessageEventContent,
        },
    },
};
use tracing::{error, info, warn};

use crate::{cluster::Cluster, commands};

pub async fn on_invite(event: StrippedRoomMemberEvent, client: Client, room: Room) {
    if Some(event.state_key.as_ref()) != client.user_id() {
        return;
    }
    info!(room = %room.room_id(), inviter = %event.sender, "invited");
    if let Err(e) = room.join().await {
        warn!(room = %room.room_id(), "join failed: {e}");
    }
}

pub async fn on_message(
    event: OriginalSyncRoomMessageEvent,
    client: Client,
    room: Room,
    Ctx(cluster): Ctx<Cluster>,
) {
    if room.state() != RoomState::Joined || Some(event.sender.as_ref()) == client.user_id() {
        return;
    }
    let MessageType::Text(text) = &event.content.msgtype else {
        return;
    };
    let body = text.body.clone();

    // Commands run in their own task so a slow one doesn't stall the sync loop.
    tokio::spawn(async move {
        let sender = event.sender.clone();
        let reply = commands::dispatch(&body, client, room.clone(), sender, cluster).await;
        let Some(reply) = reply else {
            return;
        };
        let content = RoomMessageEventContent::text_markdown(reply).make_reply_to(
            &event,
            ForwardThread::Yes,
            AddMentions::No,
        );
        if let Err(e) = room.send(content).await {
            error!(room = %room.room_id(), "send failed: {e}");
        }
    });
}
