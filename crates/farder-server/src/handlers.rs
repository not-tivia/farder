use crate::{
    audit, channels,
    events::{BroadcastEvent, EventTarget},
    invites, members, messages, permissions,
    state::ServerState,
};
use anyhow::Result;
use farder_crypto::event_log::EventPayload;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::*;
use rusqlite::Connection;
use serde_json::json;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Output type
// ---------------------------------------------------------------------------

pub struct HandleResult {
    pub response: ServerResponse,
    pub events: Vec<BroadcastEvent>,
    pub orphaned_file_ids: Vec<u64>,
}

// ---------------------------------------------------------------------------
// Helper constructors
// ---------------------------------------------------------------------------

fn ok(response: ServerResponse) -> Result<HandleResult> {
    Ok(HandleResult {
        response,
        events: vec![],
        orphaned_file_ids: vec![],
    })
}

fn ok_with(response: ServerResponse, events: Vec<BroadcastEvent>) -> Result<HandleResult> {
    Ok(HandleResult { response, events, orphaned_file_ids: vec![] })
}

fn err(reason: &str) -> Result<HandleResult> {
    Ok(HandleResult {
        response: ServerResponse::Error {
            reason: reason.to_string(),
        },
        events: vec![],
        orphaned_file_ids: vec![],
    })
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------------------------------------------------------------------------
// Shared MemberInfo builder
// ---------------------------------------------------------------------------

/// Build a `MemberInfo` for the given public key, suitable for inclusion in
/// `DmCreated`, `DmOpened`, etc.  Reads the member record, role ids, timeout
/// state, and live presence from `state`.  Returns an error if the member does
/// not exist.
pub fn build_member_info(
    conn: &Connection,
    state: &crate::state::ServerState,
    pk: &PublicKey,
) -> Result<MemberInfo> {
    let record = members::get_member(conn, pk)?
        .ok_or_else(|| anyhow::anyhow!("member not found: {:?}", pk))?;
    let role_ids = members::get_member_role_ids(conn, pk)?;
    let (timeout_until, timeout_reason) = match members::is_timed_out(conn, pk, current_unix_ms())? {
        Some((until, reason)) => (Some(until), reason),
        None => (None, None),
    };
    Ok(MemberInfo {
        public_key: pk.clone(),
        display_name: record.display_name,
        joined_at: record.joined_at,
        role_ids,
        timeout_until,
        timeout_reason,
        profile_hash: record.profile_hash,
        presence: state.presences.read().unwrap().get(pk.as_bytes()).cloned(),
        is_bot: record.is_bot,
    })
}

// ---------------------------------------------------------------------------
// Permission resolution helper
// ---------------------------------------------------------------------------

pub fn resolve_member_perms_pub(
    conn: &Connection,
    member: &PublicKey,
    channel_id: u64,
    is_owner: bool,
) -> Result<u64> {
    resolve_member_perms(conn, member, channel_id, is_owner)
}

/// Channel-level authorization check for RunCommand dispatch.
///
/// Returns `Ok(None)` if the caller is allowed to post to `channel_id`, or
/// `Ok(Some(reason))` if they are not.  Mirrors the `SendMessage` handler:
/// - Rejects timed-out members.
/// - For DM channels: the caller must be a participant.
/// - For all other channels: the caller must have `SEND_MESSAGES`.
pub fn check_run_command_channel_auth(
    conn: &Connection,
    member: &PublicKey,
    is_owner: bool,
    channel_id: u64,
) -> Result<Option<String>> {
    // 1. Timeout check (same helper that SendMessage uses internally).
    if let Some((until_ms, reason)) = members::is_timed_out(conn, member, current_unix_ms())? {
        let reason_part = reason.map(|r| format!(": {r}")).unwrap_or_default();
        return Ok(Some(format!("timed out until {until_ms}{reason_part}")));
    }

    // 2. Channel-level check (mirrors SendMessage).
    let channel = channels::get_channel(conn, channel_id)?
        .ok_or_else(|| anyhow::anyhow!("channel not found"))?;

    if channel.channel_type == ChannelType::Dm {
        // DM: caller must be a participant.
        if !channels::is_dm_participant(conn, channel_id, member)? {
            return Ok(Some("not a participant in this DM".to_string()));
        }
    } else {
        // Normal channel: caller must have SEND_MESSAGES.
        let perms = resolve_member_perms_pub(conn, member, channel_id, is_owner)?;
        if !permissions::has(perms, permissions::SEND_MESSAGES) {
            return Ok(Some("missing SEND_MESSAGES permission".to_string()));
        }
    }

    Ok(None)
}

/// Server-level permissions only (no channel/category overrides).
pub fn resolve_member_server_perms(
    conn: &Connection,
    member: &PublicKey,
    is_owner: bool,
) -> Result<u64> {
    if is_owner {
        return Ok(permissions::ALL_PERMISSIONS);
    }
    let everyone_perms: u64 = conn
        .query_row(
            "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
            [],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )
        .unwrap_or(0);
    let role_perms = members::get_member_role_permissions(conn, member)?;
    let ctx = permissions::ResolutionContext {
        everyone_permissions: everyone_perms,
        role_permissions: role_perms,
        category_overrides: vec![],
        channel_overrides: vec![],
        is_owner,
    };
    Ok(permissions::resolve(ctx))
}

fn resolve_member_perms(
    conn: &Connection,
    member: &PublicKey,
    channel_id: u64,
    is_owner: bool,
) -> Result<u64> {
    if is_owner {
        return Ok(permissions::ALL_PERMISSIONS);
    }

    // 1. Get member's role IDs.
    let role_ids = members::get_member_role_ids(conn, member)?;

    // 2. Get @everyone permissions from the builtin role.
    let everyone_perms: u64 = conn
        .query_row(
            "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
            [],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )
        .unwrap_or(0);

    // 3. Get all role permissions for this member.
    let role_perms = members::get_member_role_permissions(conn, member)?;

    // 4. Get channel info to find category_id.
    let channel = channels::get_channel(conn, channel_id)?;
    let category_id = channel.and_then(|ch| ch.category_id);

    // 5. Get category overrides for member's roles (if channel has a category).
    let category_overrides = if let Some(cat_id) = category_id {
        let ovs = channels::get_category_overrides_for_roles(conn, cat_id, &role_ids)?;
        ovs.into_iter()
            .map(|o| permissions::Override {
                allow: o.allow,
                deny: o.deny,
            })
            .collect()
    } else {
        vec![]
    };

    // 6. Get channel overrides for member's roles.
    let channel_ovs = channels::get_channel_overrides_for_roles(conn, channel_id, &role_ids)?;
    let channel_overrides: Vec<permissions::Override> = channel_ovs
        .into_iter()
        .map(|o| permissions::Override {
            allow: o.allow,
            deny: o.deny,
        })
        .collect();

    // 7. Build context and resolve.
    let ctx = permissions::ResolutionContext {
        everyone_permissions: everyone_perms,
        role_permissions: role_perms,
        category_overrides,
        channel_overrides,
        is_owner,
    };

    Ok(permissions::resolve(ctx))
}

fn resolve_base_perms(conn: &Connection, member: &PublicKey, is_owner: bool) -> Result<u64> {
    if is_owner {
        return Ok(permissions::ALL_PERMISSIONS);
    }
    let everyone_perms: u64 = conn
        .query_row(
            "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
            [],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )
        .unwrap_or(0);
    let role_perms = members::get_member_role_permissions(conn, member)?;
    let mut base = everyone_perms;
    for rp in &role_perms {
        base |= rp;
    }
    Ok(base)
}

fn require_base_perm(
    conn: &Connection,
    member: &PublicKey,
    is_owner: bool,
    perm: u64,
    perm_name: &str,
) -> Result<Option<HandleResult>> {
    let base = resolve_base_perms(conn, member, is_owner)?;
    if !permissions::has(base, perm) {
        return Ok(Some(HandleResult {
            response: ServerResponse::Error {
                reason: format!("missing {} permission", perm_name),
            },
            events: Vec::new(),
            orphaned_file_ids: vec![],
        }));
    }
    Ok(None)
}

/// Check that the actor's highest role position is above the target position.
/// Owners bypass this check. Returns an error response if the check fails.
fn require_role_hierarchy(
    conn: &Connection,
    actor: &PublicKey,
    is_owner: bool,
    target_position: u32,
) -> Result<Option<HandleResult>> {
    if is_owner {
        return Ok(None);
    }
    let actor_pos = members::get_highest_role_position(conn, actor)?;
    if actor_pos <= target_position {
        return Ok(Some(HandleResult {
            response: ServerResponse::Error {
                reason: "cannot manage roles at or above your own position".to_string(),
            },
            events: Vec::new(),
            orphaned_file_ids: vec![],
        }));
    }
    Ok(None)
}

/// Insert an audit row and return a `BroadcastEvent` envelope (PermissionHolders(MANAGE_SERVER))
/// for the matching `AuditEventCreated` event. Returns `None` (and logs) on insert failure so the
/// caller's primary mutation still completes.
fn audit_emit(
    conn: &Connection,
    actor: &PublicKey,
    target: Option<&PublicKey>,
    action: &str,
    metadata: serde_json::Value,
) -> Option<BroadcastEvent> {
    match audit::insert(conn, actor, target, action, metadata) {
        Ok(event) => Some(BroadcastEvent {
            target: EventTarget::PermissionHolders(permissions::MANAGE_SERVER),
            event: ServerEvent::AuditEventCreated { event },
        }),
        Err(e) => {
            eprintln!("[audit] insert failed: {e}");
            None
        }
    }
}

fn require_not_timed_out(conn: &Connection, member: &PublicKey) -> Result<Option<HandleResult>> {
    if let Some((until_ms, reason)) = members::is_timed_out(conn, member, current_unix_ms())? {
        let reason_part = reason.map(|r| format!(": {r}")).unwrap_or_default();
        return Ok(Some(HandleResult {
            response: ServerResponse::Error {
                reason: format!("timed out until {until_ms}{reason_part}"),
            },
            events: Vec::new(),
            orphaned_file_ids: vec![],
        }));
    }
    Ok(None)
}

/// Check that the actor outranks the target member.
fn require_member_hierarchy(
    conn: &Connection,
    actor: &PublicKey,
    is_owner: bool,
    target: &PublicKey,
) -> Result<Option<HandleResult>> {
    if is_owner {
        return Ok(None);
    }
    let actor_pos = members::get_highest_role_position(conn, actor)?;
    let target_pos = members::get_highest_role_position(conn, target)?;
    if actor_pos <= target_pos {
        return Ok(Some(HandleResult {
            response: ServerResponse::Error {
                reason: "cannot manage members at or above your own role level".to_string(),
            },
            events: Vec::new(),
            orphaned_file_ids: vec![],
        }));
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Mesh content gate
// ---------------------------------------------------------------------------

/// Whether a request requires the caller to be a LOG member on a mesh server.
/// Default-deny: everything is gated EXCEPT a small bootstrap allow-list that a
/// not-yet-member must be able to call — submit their join/device events, resolve
/// an invite code, and read server info. Adding to this list is a deliberate act.
fn request_requires_membership(req: &ServerRequest) -> bool {
    !matches!(
        req,
        ServerRequest::SubmitEvent { .. }
            | ServerRequest::ResolveInvite { .. }
            | ServerRequest::GetServerInfo
            | ServerRequest::GetMembershipStatus
    )
}

/// On a mesh server (event log present), returns `Some(reason)` if `member` is
/// NOT a log member — meaning content must be denied. Returns `None` when the
/// caller is a log member OR this is a legacy server (no log). The reason
/// distinguishes a pending-approval join from a plain non-member (for client UX).
pub fn content_block_reason(state: &crate::state::ServerState, member: &PublicKey) -> Option<String> {
    let guard = state.log_state.lock().unwrap();
    match guard.as_ref() {
        Some(ls) if !ls.is_member(member) => Some(if ls.is_pending(member) {
            "pending approval: waiting for a moderator to approve your join".to_string()
        } else {
            "not a member of this server".to_string()
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Main dispatch
// ---------------------------------------------------------------------------

pub fn handle_request(
    conn: &Connection,
    member: &PublicKey,
    is_owner: bool,
    request: ServerRequest,
    _storage_dir: &str,
    state: &Arc<ServerState>,
) -> Result<HandleResult> {
    // Mesh content gate: when this server has an event log, the log is
    // authoritative for membership. A caller who is not a log member may only
    // make bootstrap requests (submit join events, resolve an invite, read
    // server info); everything else is rejected. Legacy servers (no log) skip
    // this entirely.
    if request_requires_membership(&request) {
        if let Some(reason) = content_block_reason(state, member) {
            return err(&reason);
        }
    }

    match request {
        // ----------------------------------------------------------------
        // Messaging
        // ----------------------------------------------------------------
        ServerRequest::SendMessage {
            channel_id,
            content,
            reply_to,
            attachment_ids,
        } => {
            if let Some(denied) = require_not_timed_out(conn, member)? {
                return Ok(denied);
            }
            if content.len() > 8000 {
                return err("message content too long (max 8000 characters)");
            }
            if attachment_ids.len() > 10 {
                return err("too many attachments (max 10)");
            }

            let channel = channels::get_channel(conn, channel_id)?
                .ok_or_else(|| anyhow::anyhow!("channel not found"))?;

            if channel.channel_type == ChannelType::Dm {
                // DM: check participation and blocks.
                if !channels::is_dm_participant(conn, channel_id, member)? {
                    return err("not a participant in this DM");
                }
                let others = channels::list_dm_channels(conn, member)?;
                if let Some((_, other_key)) = others.iter().find(|(ch, _)| ch.id == channel_id) {
                    if members::is_blocked(conn, member, other_key)? {
                        return err("this user is blocked");
                    }
                }
            } else {
                // Normal channel: check permissions.
                let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
                if !permissions::has(perms, permissions::SEND_MESSAGES) {
                    return err("missing SEND_MESSAGES permission");
                }
            }

            let id = messages::insert_message(conn, channel_id, member, &content, reply_to)?;

            // Create attachment records
            for (pos, file_id) in attachment_ids.iter().enumerate() {
                let file = crate::attachments::get_file(conn, *file_id)?
                    .ok_or_else(|| anyhow::anyhow!("attachment file_id {} not found", file_id))?;
                if file.uploaded_by != *member && !is_owner {
                    return err("cannot attach files uploaded by another member");
                }
                crate::attachments::create_message_attachment(
                    conn, id, *file_id, pos as u32, &file.original_name,
                    file.width, file.height, file.duration_secs,
                )?;
            }

            let msg = match messages::get_message(conn, id, member)? {
                Some(m) => m,
                None => return err("failed to retrieve sent message"),
            };
            let timestamp = msg.timestamp;
            let event = BroadcastEvent {
                target: EventTarget::Subscribers(channel_id),
                event: ServerEvent::NewMessage { message: msg },
            };
            ok_with(
                ServerResponse::MessageSent { id, timestamp },
                vec![event],
            )
        }

        ServerRequest::EditMessage {
            message_id,
            new_content,
        } => {
            if new_content.len() > 8000 {
                return err("message content too long (max 8000 characters)");
            }
            let msg = match messages::get_message(conn, message_id, member)? {
                Some(m) => m,
                None => return err("message not found"),
            };
            if msg.author != *member {
                return err("can only edit own messages");
            }
            messages::edit_message(conn, message_id, &new_content)?;
            let updated = messages::get_message(conn, message_id, member)?.unwrap();
            let edited_at = updated.edited_at.unwrap_or(0);
            let channel_id = msg.channel_id;
            let event = BroadcastEvent {
                target: EventTarget::Subscribers(channel_id),
                event: ServerEvent::MessageEdited {
                    message_id,
                    channel_id,
                    new_content,
                    edited_at,
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::DeleteMessage { message_id } => {
            let msg = match messages::get_message(conn, message_id, member)? {
                Some(m) => m,
                None => return err("message not found"),
            };
            let channel_id = msg.channel_id;

            if msg.author != *member {
                // Check MANAGE_MESSAGES
                let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
                if !permissions::has(perms, permissions::MANAGE_MESSAGES) {
                    return err("missing MANAGE_MESSAGES permission");
                }
            }

            let orphans = messages::delete_message(conn, message_id)?;
            let event = BroadcastEvent {
                target: EventTarget::Subscribers(channel_id),
                event: ServerEvent::MessageDeleted {
                    message_id,
                    channel_id,
                },
            };
            Ok(HandleResult {
                response: ServerResponse::Ok,
                events: vec![event],
                orphaned_file_ids: orphans,
            })
        }

        ServerRequest::FetchHistory {
            channel_id,
            before_id,
            limit,
        } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::READ_MESSAGES) {
                return err("missing READ_MESSAGES permission");
            }
            let limit = limit.min(500);
            let msgs = messages::fetch_history(conn, channel_id, before_id, limit, member)?;
            ok(ServerResponse::History { messages: msgs })
        }

        ServerRequest::PinMessage { message_id } => {
            let msg = match messages::get_message(conn, message_id, member)? {
                Some(m) => m,
                None => return err("message not found"),
            };
            let channel_id = msg.channel_id;
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::MANAGE_MESSAGES) {
                return err("missing MANAGE_MESSAGES permission");
            }
            messages::pin_message(conn, message_id)?;
            let event = BroadcastEvent {
                target: EventTarget::Subscribers(channel_id),
                event: ServerEvent::MessagePinned {
                    message_id,
                    channel_id,
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::UnpinMessage { message_id } => {
            let msg = match messages::get_message(conn, message_id, member)? {
                Some(m) => m,
                None => return err("message not found"),
            };
            let channel_id = msg.channel_id;
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::MANAGE_MESSAGES) {
                return err("missing MANAGE_MESSAGES permission");
            }
            messages::unpin_message(conn, message_id)?;
            let event = BroadcastEvent {
                target: EventTarget::Subscribers(channel_id),
                event: ServerEvent::MessageUnpinned {
                    message_id,
                    channel_id,
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::Search {
            query,
            channel_id,
            limit,
        } => {
            let limit = limit.min(500);
            if let Some(cid) = channel_id {
                let perms = resolve_member_perms(conn, member, cid, is_owner)?;
                if !permissions::has(perms, permissions::READ_MESSAGES) {
                    return err("missing READ_MESSAGES permission");
                }
            }
            let mut msgs = messages::search_messages(conn, &query, channel_id, limit, member)?;
            if channel_id.is_none() && !is_owner {
                // Filter results to channels the member can read.
                msgs.retain(|msg| {
                    resolve_member_perms(conn, member, msg.channel_id, is_owner)
                        .map(|p| {
                            permissions::has(p, permissions::READ_MESSAGES)
                                && permissions::has(p, permissions::VIEW_CHANNEL)
                        })
                        .unwrap_or(false)
                });
            }
            ok(ServerResponse::SearchResults { messages: msgs })
        }

        ServerRequest::Typing { channel_id } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::SEND_MESSAGES) {
                return err("missing SEND_MESSAGES permission");
            }
            let event = BroadcastEvent {
                target: EventTarget::Subscribers(channel_id),
                event: ServerEvent::TypingStarted {
                    channel_id,
                    public_key: member.clone(),
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        // ----------------------------------------------------------------
        // Channel management
        // ----------------------------------------------------------------
        ServerRequest::CreateChannel {
            name,
            channel_type,
            category_id,
            position,
        } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_CHANNEL, "MANAGE_CHANNEL")? {
                return Ok(denied);
            }
            let pos = position.unwrap_or(0);
            let channel_id =
                channels::create_channel(conn, &name, channel_type, category_id, pos)?;
            let channel = channels::get_channel(conn, channel_id)?.unwrap();
            let audit_meta = json!({
                "channel_id": channel.id,
                "channel_name": channel.name,
                "channel_type": format!("{:?}", channel.channel_type),
            });
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::ChannelCreated {
                    channel: channel.clone(),
                },
            }];
            if let Some(audit_evt) = audit_emit(conn, member, None, "channel_created", audit_meta) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::UpdateChannel {
            channel_id,
            name,
            topic,
            nsfw,
            slow_mode_secs,
            retention_secs,
            category_id,
            position,
        } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::MANAGE_CHANNEL) {
                return err("missing MANAGE_CHANNEL permission");
            }
            // Capture pre-update name so we can detect actual rename for audit.
            let prev_name = channels::get_channel(conn, channel_id)?
                .map(|ch| ch.name)
                .unwrap_or_default();
            channels::update_channel(
                conn,
                channel_id,
                name.as_deref(),
                topic.as_deref(),
                nsfw,
                slow_mode_secs,
                retention_secs,
                category_id,
                position,
            )?;
            let channel = channels::get_channel(conn, channel_id)?.unwrap();
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::ChannelUpdated {
                    channel: channel.clone(),
                },
            }];
            // Only audit when the channel was actually renamed.
            if let Some(new_name) = name.as_deref() {
                if new_name != prev_name {
                    if let Some(audit_evt) = audit_emit(
                        conn,
                        member,
                        None,
                        "channel_renamed",
                        json!({
                            "channel_id": channel_id,
                            "old_name": prev_name,
                            "new_name": new_name,
                        }),
                    ) {
                        events.push(audit_evt);
                    }
                }
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::DeleteChannel { channel_id } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::MANAGE_CHANNEL) {
                return err("missing MANAGE_CHANNEL permission");
            }
            // Capture name BEFORE delete so the audit row has it.
            let prev_channel_name = channels::get_channel(conn, channel_id)?
                .map(|ch| ch.name)
                .unwrap_or_default();
            channels::soft_delete_channel(conn, channel_id)?;
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::ChannelDeleted { channel_id },
            }];
            if let Some(audit_evt) = audit_emit(
                conn,
                member,
                None,
                "channel_deleted",
                json!({ "channel_id": channel_id, "channel_name": prev_channel_name }),
            ) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
        }

        // ----------------------------------------------------------------
        // Category management
        // ----------------------------------------------------------------
        ServerRequest::CreateCategory { name, position } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            let pos = position.unwrap_or(0);
            let category_id = channels::create_category(conn, &name, pos)?;
            let category = channels::get_category(conn, category_id)?.unwrap();
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::CategoryCreated {
                    category: category.clone(),
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::UpdateCategory {
            category_id,
            name,
            position,
        } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            channels::update_category(conn, category_id, name.as_deref(), position)?;
            let category = channels::get_category(conn, category_id)?.unwrap();
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::CategoryUpdated {
                    category: category.clone(),
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::DeleteCategory { category_id } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            channels::delete_category(conn, category_id)?;
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::CategoryDeleted { category_id },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        // ----------------------------------------------------------------
        // Role management
        // ----------------------------------------------------------------
        ServerRequest::CreateRole {
            name,
            permissions: perms_bits,
            color,
            position,
            hoist,
        } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_ROLES, "MANAGE_ROLES")? {
                return Ok(denied);
            }
            let pos = position.unwrap_or(0);
            if let Some(denied) = require_role_hierarchy(conn, member, is_owner, pos)? {
                return Ok(denied);
            }
            let role_id = members::create_role(
                conn,
                &name,
                perms_bits,
                color.as_deref(),
                pos,
                false,
                hoist.unwrap_or(false),
            )?;
            let role = members::get_role(conn, role_id)?.unwrap();
            let audit_meta = json!({
                "role_id": role.id,
                "role_name": role.name,
                "permissions": role.permissions.to_string(),
            });
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::RoleCreated { role: role.clone() },
            }];
            if let Some(audit_evt) = audit_emit(conn, member, None, "role_created", audit_meta) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::UpdateRole {
            role_id,
            name,
            permissions: perms_bits,
            color,
            position,
            hoist,
        } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_ROLES, "MANAGE_ROLES")? {
                return Ok(denied);
            }
            let target_role = members::get_role(conn, role_id)?.ok_or_else(|| anyhow::anyhow!("role not found"))?;
            if let Some(denied) = require_role_hierarchy(conn, member, is_owner, target_role.position)? {
                return Ok(denied);
            }
            if let Some(new_pos) = position {
                if let Some(denied) = require_role_hierarchy(conn, member, is_owner, new_pos)? {
                    return Ok(denied);
                }
            }
            // Capture prior permissions BEFORE update so we can detect a real change.
            let prev_perms = target_role.permissions;
            // color field in UpdateRole is Option<String> (not Option<Option<String>>),
            // so we pass it as Some(color.as_deref()) when present, None when absent.
            let color_param: Option<Option<&str>> = color.as_ref().map(|c| Some(c.as_str()));
            members::update_role(
                conn,
                role_id,
                name.as_deref(),
                perms_bits,
                color_param,
                position,
                hoist,
            )?;
            let role = members::get_role(conn, role_id)?.unwrap();
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::RoleUpdated { role: role.clone() },
            }];
            // Only audit when the permissions field was provided AND actually changed.
            if let Some(new_perms) = perms_bits {
                if new_perms != prev_perms {
                    if let Some(audit_evt) = audit_emit(
                        conn,
                        member,
                        None,
                        "role_perms_changed",
                        json!({
                            "role_id": role_id,
                            "old_permissions": prev_perms.to_string(),
                            "new_permissions": new_perms.to_string(),
                        }),
                    ) {
                        events.push(audit_evt);
                    }
                }
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::DeleteRole { role_id } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_ROLES, "MANAGE_ROLES")? {
                return Ok(denied);
            }
            let target_role = members::get_role(conn, role_id)?.ok_or_else(|| anyhow::anyhow!("role not found"))?;
            if let Some(denied) = require_role_hierarchy(conn, member, is_owner, target_role.position)? {
                return Ok(denied);
            }
            // Capture name BEFORE delete.
            let prev_name = target_role.name.clone();
            members::delete_role(conn, role_id)?;
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::RoleDeleted { role_id },
            }];
            if let Some(audit_evt) = audit_emit(
                conn,
                member,
                None,
                "role_deleted",
                json!({ "role_id": role_id, "role_name": prev_name }),
            ) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::AssignRole { member_key, role_id } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_ROLES, "MANAGE_ROLES")? {
                return Ok(denied);
            }
            let target_role = members::get_role(conn, role_id)?.ok_or_else(|| anyhow::anyhow!("role not found"))?;
            if let Some(denied) = require_role_hierarchy(conn, member, is_owner, target_role.position)? {
                return Ok(denied);
            }
            members::assign_role(conn, &member_key, role_id)?;
            let role_name = target_role.name.clone();
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::PermissionsChanged,
            }];
            if let Some(audit_evt) = audit_emit(
                conn,
                member,
                Some(&member_key),
                "role_assigned",
                json!({ "role_id": role_id, "role_name": role_name }),
            ) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::RemoveRole { member_key, role_id } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_ROLES, "MANAGE_ROLES")? {
                return Ok(denied);
            }
            let target_role = members::get_role(conn, role_id)?.ok_or_else(|| anyhow::anyhow!("role not found"))?;
            if let Some(denied) = require_role_hierarchy(conn, member, is_owner, target_role.position)? {
                return Ok(denied);
            }
            members::unassign_role(conn, &member_key, role_id)?;
            let role_name = target_role.name.clone();
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::PermissionsChanged,
            }];
            if let Some(audit_evt) = audit_emit(
                conn,
                member,
                Some(&member_key),
                "role_removed",
                json!({ "role_id": role_id, "role_name": role_name }),
            ) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
        }

        // ----------------------------------------------------------------
        // Member management
        // ----------------------------------------------------------------
        ServerRequest::KickMember { member_key } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::KICK_MEMBERS, "KICK_MEMBERS")? {
                return Ok(denied);
            }
            if let Some(denied) = require_member_hierarchy(conn, member, is_owner, &member_key)? {
                return Ok(denied);
            }
            members::remove_member(conn, &member_key)?;
            let mut events = vec![BroadcastEvent {
                target: EventTarget::Members(vec![member_key.clone()]),
                event: ServerEvent::YouWereKicked,
            }];
            events.push(BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberLeft {
                    public_key: member_key.clone(),
                },
            });
            if let Some(audit_evt) = audit_emit(conn, member, Some(&member_key), "kick", json!({})) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::BanMember { member_key, reason } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::BAN_MEMBERS, "BAN_MEMBERS")? {
                return Ok(denied);
            }
            if let Some(denied) = require_member_hierarchy(conn, member, is_owner, &member_key)? {
                return Ok(denied);
            }
            members::ban_member(conn, &member_key, reason.as_deref())?;
            let reason_for_audit = reason.clone();
            let reason_for_target_event = reason.clone();
            let target_for_audit = member_key.clone();
            let target_for_kicked_event = member_key.clone();
            let mut events = vec![BroadcastEvent {
                target: EventTarget::Members(vec![target_for_kicked_event]),
                event: ServerEvent::YouWereBanned {
                    reason: reason_for_target_event,
                },
            }];
            events.push(BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberBanned {
                    public_key: member_key,
                    reason,
                },
            });
            if let Some(audit_evt) = audit_emit(
                conn,
                member,
                Some(&target_for_audit),
                "ban",
                json!({ "reason": reason_for_audit }),
            ) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
        }

        // ----------------------------------------------------------------
        // Unban / List banned
        // ----------------------------------------------------------------
        ServerRequest::UnbanMember { member_key } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::BAN_MEMBERS, "BAN_MEMBERS")? {
                return Ok(denied);
            }
            members::unban_member(conn, &member_key)?;
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberUnbanned { public_key: member_key.clone() },
            }];
            if let Some(audit_evt) = audit_emit(conn, member, Some(&member_key), "unban", json!({})) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::ListBanned => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::BAN_MEMBERS, "BAN_MEMBERS")? {
                return Ok(denied);
            }
            let entries = members::list_banned(conn)?;
            ok(ServerResponse::BannedMembers { entries })
        }

        // ----------------------------------------------------------------
        // Invites
        // ----------------------------------------------------------------
        ServerRequest::ResolveInvite { code } => {
            let invite_event = crate::event_ingest::find_invite_event_by_code(conn, &code)?;
            ok(ServerResponse::InviteResolved { invite_event })
        }

        ServerRequest::CreateInvite {
            max_uses,
            expires_in_secs,
            target_channel,
        } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::CREATE_INVITES, "CREATE_INVITES")? {
                return Ok(denied);
            }
            let code =
                invites::create_invite(conn, member, max_uses, expires_in_secs, target_channel)?;
            ok(ServerResponse::InviteCreated { code })
        }

        // ----------------------------------------------------------------
        // Info queries
        // ----------------------------------------------------------------
        ServerRequest::GetServerInfo => {
            let all_members = members::list_members(conn)?;
            let member_count = all_members.len() as u32;
            let is_member = {
                let guard = state.log_state.lock().unwrap();
                guard.as_ref().map(|ls| ls.is_member(member)).unwrap_or(true) // legacy: full info
            };
            let (channels_list, categories_list, roles_list) = if is_member {
                (channels::list_channels(conn)?, channels::list_categories(conn)?, members::list_roles(conn)?)
            } else {
                (Vec::new(), Vec::new(), Vec::new())
            };
            ok(ServerResponse::ServerInfo {
                name: String::new(), // patched by connection handler
                member_count,
                channels: channels_list,
                categories: categories_list,
                roles: roles_list,
                owner_public_key: None, // patched by connection handler
                server_id: state.genesis.lock().unwrap().as_ref().map(|g| g.server_id()),
            })
        }

        ServerRequest::GetMembershipStatus => {
            let status = {
                let guard = state.log_state.lock().unwrap();
                match guard.as_ref() {
                    Some(ls) if ls.is_member(member) => "member",
                    Some(ls) if ls.is_pending(member) => "pending",
                    _ => "none",
                }
            };
            ok(ServerResponse::MembershipStatus { status: status.to_string() })
        }

        ServerRequest::GetMembers => {
            let mut all_members = members::list_members(conn)?;
            // Mesh server: the log is authoritative — hide anyone not yet a log member
            // (e.g. a pending-approval join). Legacy servers keep the full list.
            {
                let guard = state.log_state.lock().unwrap();
                if let Some(ls) = guard.as_ref() {
                    all_members.retain(|m| m.is_bot || ls.is_member(&m.public_key));
                }
            }
            let now_ms = current_unix_ms();
            let presences = state.presences.read().unwrap();
            let mut member_infos: Vec<MemberInfo> = Vec::new();
            for m in all_members {
                let role_ids = members::get_member_role_ids(conn, &m.public_key)?;
                let (timeout_until, timeout_reason) = match members::is_timed_out(conn, &m.public_key, now_ms)? {
                    Some((until, reason)) => (Some(until), reason),
                    None => (None, None),
                };
                let profile_hash = m.profile_hash.clone();
                let presence = presences.get(m.public_key.as_bytes()).cloned();
                let is_bot = m.is_bot;
                member_infos.push(MemberInfo {
                    public_key: m.public_key,
                    display_name: m.display_name,
                    joined_at: m.joined_at,
                    role_ids,
                    timeout_until,
                    timeout_reason,
                    profile_hash,
                    presence,
                    is_bot,
                });
            }
            ok(ServerResponse::Members {
                members: member_infos,
            })
        }

        ServerRequest::GetPendingMembers => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::KICK_MEMBERS, "KICK_MEMBERS")? {
                return Ok(denied);
            }
            let pending_pks = {
                let guard = state.log_state.lock().unwrap();
                guard.as_ref().map(|ls| ls.pending_members()).unwrap_or_default()
            };
            let mut members_out = Vec::new();
            for pk in pending_pks {
                if let Ok(Some(rec)) = members::get_member(conn, &pk) {
                    members_out.push(MemberInfo {
                        public_key: pk,
                        display_name: rec.display_name,
                        joined_at: rec.joined_at,
                        role_ids: vec![],
                        timeout_until: None,
                        timeout_reason: None,
                        profile_hash: rec.profile_hash,
                        presence: None,
                        is_bot: rec.is_bot,
                    });
                }
            }
            ok(ServerResponse::PendingMembers { members: members_out })
        }

        // ----------------------------------------------------------------
        // Override management
        // ----------------------------------------------------------------
        ServerRequest::SetChannelOverride {
            channel_id,
            role_id,
            allow,
            deny,
        } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::MANAGE_CHANNEL) {
                return err("missing MANAGE_CHANNEL permission");
            }
            channels::set_channel_override(conn, channel_id, role_id, allow, deny)?;
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::PermissionsChanged,
            }];
            if let Some(audit_evt) = audit_emit(
                conn,
                member,
                None,
                "channel_overrides_changed",
                json!({
                    "channel_id": channel_id,
                    "role_id": role_id,
                    "allow": allow.to_string(),
                    "deny": deny.to_string(),
                }),
            ) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::SetCategoryOverride {
            category_id,
            role_id,
            allow,
            deny,
        } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            channels::set_category_override(conn, category_id, role_id, allow, deny)?;
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::PermissionsChanged,
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        // ----------------------------------------------------------------
        // Subscribe (connection-level, handler just acks)
        // ----------------------------------------------------------------
        ServerRequest::Subscribe { .. } => ok(ServerResponse::Ok),

        // ----------------------------------------------------------------
        // Threads and reactions
        // ----------------------------------------------------------------
        ServerRequest::CreateThread { message_id, name } => {
            let msg = messages::get_message(conn, message_id, member)?
                .ok_or_else(|| anyhow::anyhow!("message not found"))?;
            let perms = resolve_member_perms(conn, member, msg.channel_id, is_owner)?;
            if !permissions::has(perms, permissions::SEND_MESSAGES) {
                return err("missing SEND_MESSAGES permission");
            }
            let parent_channel = channels::get_channel(conn, msg.channel_id)?
                .ok_or_else(|| anyhow::anyhow!("parent channel not found"))?;
            if parent_channel.channel_type == ChannelType::Thread {
                return err("cannot create threads inside threads");
            }
            if crate::channels::get_thread_for_message(conn, message_id)?.is_some() {
                return err("thread already exists for this message");
            }
            let thread_name = name.unwrap_or_else(|| {
                let t: String = msg.content.chars().take(50).collect();
                if t.is_empty() { "Thread".to_string() } else { t }
            });
            let thread_id = channels::create_thread(conn, &thread_name, msg.channel_id, message_id)?;
            let thread = channels::get_channel(conn, thread_id)?.unwrap();
            ok_with(ServerResponse::Ok, vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::ChannelCreated { channel: thread },
            }])
        }

        ServerRequest::AddReaction { message_id, emoji, file_id } => {
            if let Some(denied) = require_not_timed_out(conn, member)? {
                return Ok(denied);
            }
            let msg = messages::get_message(conn, message_id, member)?
                .ok_or_else(|| anyhow::anyhow!("message not found"))?;
            let perms = resolve_member_perms(conn, member, msg.channel_id, is_owner)?;
            if !permissions::has(perms, permissions::READ_MESSAGES) {
                return err("missing READ_MESSAGES permission");
            }
            let inserted = crate::reactions::add_reaction(conn, message_id, member, &emoji, file_id)?;
            // A duplicate (INSERT OR IGNORE no-op) must NOT broadcast: phantom
            // ReactionAdded events made clients stack counts for re-clicks.
            let events = if inserted {
                vec![BroadcastEvent {
                    target: EventTarget::Subscribers(msg.channel_id),
                    event: ServerEvent::ReactionAdded {
                        message_id, channel_id: msg.channel_id, emoji, public_key: member.clone(), file_id,
                    },
                }]
            } else {
                vec![]
            };
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::RemoveReaction { message_id, emoji, file_id } => {
            let msg = messages::get_message(conn, message_id, member)?
                .ok_or_else(|| anyhow::anyhow!("message not found"))?;
            crate::reactions::remove_reaction(conn, message_id, member, &emoji, file_id)?;
            ok_with(ServerResponse::Ok, vec![BroadcastEvent {
                target: EventTarget::Subscribers(msg.channel_id),
                event: ServerEvent::ReactionRemoved {
                    message_id, channel_id: msg.channel_id, emoji, public_key: member.clone(), file_id,
                },
            }])
        }

        // ----------------------------------------------------------------
        // Data deletion (Phase 3.3)
        // ----------------------------------------------------------------
        ServerRequest::RequestDeletion => {
            if is_owner {
                return err("server owner cannot request deletion — transfer ownership first");
            }
            if members::get_deletion_request(conn, member)?.is_some() {
                return err("deletion request already pending");
            }
            members::create_deletion_request(conn, member)?;
            ok_with(ServerResponse::Ok, vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::DeletionRequested { public_key: member.clone() },
            }])
        }
        ServerRequest::CancelDeletion => {
            if members::get_deletion_request(conn, member)?.is_none() {
                return err("no pending deletion request");
            }
            members::cancel_deletion_request(conn, member)?;
            ok_with(ServerResponse::Ok, vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::DeletionCancelled { public_key: member.clone() },
            }])
        }
        ServerRequest::GetDeletionStatus => {
            let status = match members::get_deletion_request(conn, member)? {
                Some(req) => DeletionStatus { pending: true, requested_at: Some(req.requested_at), expires_at: Some(req.expires_at) },
                None => DeletionStatus { pending: false, requested_at: None, expires_at: None },
            };
            ok(ServerResponse::DeletionStatusResp { status })
        }

        // FetchUrl is handled async in connection.rs, not here
        ServerRequest::FetchUrl { .. } => {
            err("FetchUrl must be handled at the connection level")
        }

        // ----------------------------------------------------------------
        // DM and block operations
        // ----------------------------------------------------------------
        ServerRequest::OpenDm { target_key } => {
            // Cannot DM yourself.
            if *member == target_key {
                return err("cannot open a DM with yourself");
            }

            // Target must be a member.
            if members::get_member(conn, &target_key)?.is_none() {
                return err("target user is not a member");
            }

            // Check not blocked (bidirectional).
            if members::is_blocked(conn, member, &target_key)? {
                return err("this user is blocked");
            }

            // Find or create DM channel.
            let (channel_id, was_created) = channels::open_dm_channel(conn, member, &target_key)?;
            let channel = channels::get_channel(conn, channel_id)?
                .ok_or_else(|| anyhow::anyhow!("DM channel not found after creation"))?;

            // Build MemberInfo for participant (shared helper; also used by bots::send_bot_dm).
            let participant = build_member_info(conn, state, &target_key)?;

            let mut events = Vec::new();
            if was_created {
                events.push(BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::DmCreated {
                        channel: channel.clone(),
                        participant: participant.clone(),
                    },
                });
            }

            ok_with(ServerResponse::DmOpened { channel, participant }, events)
        }

        ServerRequest::ListDms => {
            let dm_list = channels::list_dm_channels(conn, member)?;

            let mut entries = Vec::new();
            for (ch, other_key) in dm_list {
                let other_record = match members::get_member(conn, &other_key)? {
                    Some(r) => r,
                    None => continue,
                };
                let role_ids = members::get_member_role_ids(conn, &other_key)?;
                let (timeout_until, timeout_reason) = match members::is_timed_out(conn, &other_key, current_unix_ms())? {
                    Some((until, reason)) => (Some(until), reason),
                    None => (None, None),
                };
                let participant = MemberInfo {
                    public_key: other_key.clone(),
                    display_name: other_record.display_name,
                    joined_at: other_record.joined_at,
                    role_ids,
                    timeout_until,
                    timeout_reason,
                    profile_hash: other_record.profile_hash.clone(),
                    presence: state.presences.read().unwrap().get(other_key.as_bytes()).cloned(),
                    is_bot: other_record.is_bot,
                };
                let ch_id = ch.id;
                let last_msgs = messages::fetch_history(conn, ch_id, None, 1, member)?;
                let last_message = last_msgs.into_iter().next();
                entries.push(DmEntry {
                    channel: ch,
                    participant,
                    last_message,
                });
            }

            // Sort by last message timestamp (most recent first). DMs with no
            // messages go last.
            entries.sort_by(|a, b| {
                let ts_a = a.last_message.as_ref().map(|m| m.timestamp).unwrap_or(0);
                let ts_b = b.last_message.as_ref().map(|m| m.timestamp).unwrap_or(0);
                ts_b.cmp(&ts_a)
            });

            ok(ServerResponse::DmList { dms: entries })
        }

        ServerRequest::BlockUser { target_key } => {
            members::block_user(conn, member, &target_key)?;
            ok(ServerResponse::Ok)
        }

        ServerRequest::UnblockUser { target_key } => {
            members::unblock_user(conn, member, &target_key)?;
            ok(ServerResponse::Ok)
        }

        ServerRequest::TimeoutMember { member_key, until_ms, reason } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::TIMEOUT_MEMBERS, "TIMEOUT_MEMBERS")? {
                return Ok(denied);
            }
            if let Some(denied) = require_member_hierarchy(conn, member, is_owner, &member_key)? {
                return Ok(denied);
            }
            let now = current_unix_ms();
            const MAX_TIMEOUT_MS: u64 = 28 * 24 * 60 * 60 * 1000;
            if until_ms <= now || until_ms > now + MAX_TIMEOUT_MS {
                return err("timeout duration out of range (must be in the future, max 28 days)");
            }
            members::set_timeout(conn, &member_key, until_ms, reason.as_deref())?;
            let reason_for_audit = reason.clone();
            let target_for_audit = member_key.clone();
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberTimeoutChanged {
                    public_key: member_key,
                    until_ms: Some(until_ms),
                    reason: reason.clone(),
                },
            }];
            if let Some(audit_evt) = audit_emit(
                conn,
                member,
                Some(&target_for_audit),
                "timeout",
                json!({ "until_ms": until_ms, "reason": reason_for_audit }),
            ) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::RemoveTimeout { member_key } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::TIMEOUT_MEMBERS, "TIMEOUT_MEMBERS")? {
                return Ok(denied);
            }
            if let Some(denied) = require_member_hierarchy(conn, member, is_owner, &member_key)? {
                return Ok(denied);
            }
            members::clear_timeout(conn, &member_key)?;
            let target_for_audit = member_key.clone();
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberTimeoutChanged {
                    public_key: member_key,
                    until_ms: None,
                    reason: None,
                },
            }];
            if let Some(audit_evt) = audit_emit(conn, member, Some(&target_for_audit), "untimeout", json!({})) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::ListAuditEvents { before_id, limit } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            let events = audit::list(conn, before_id, limit)?;
            ok(ServerResponse::AuditEventsList { events })
        }

        // Lobby-presence arms: join/leave/query the media channel participant list.
        ServerRequest::JoinChannelMedia { channel_id } => {
            if let Some(denied) = require_not_timed_out(conn, member)? {
                return Ok(denied);
            }
            let channel = channels::get_channel(conn, channel_id)?
                .ok_or_else(|| anyhow::anyhow!("channel not found"))?;
            if channel.channel_type != ChannelType::Voice {
                return err("not a voice channel");
            }
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::CONNECT) {
                return err("missing CONNECT permission");
            }
            let left_channels = channels::leave_all_voice(conn, member)?;
            let mut events: Vec<BroadcastEvent> = Vec::new();
            for left_ch in left_channels {
                events.push(BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::MediaLeft { channel_id: left_ch, public_key: member.clone() },
                });
            }
            channels::join_voice(conn, channel_id, member)?;
            let display_name = members::get_member(conn, member)?
                .map(|m| m.display_name).unwrap_or_default();
            events.push(BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MediaJoined { channel_id, public_key: member.clone(), display_name },
            });
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::LeaveChannelMedia { channel_id } => {
            let channel = channels::get_channel(conn, channel_id)?;
            channels::leave_voice(conn, channel_id, member)?;
            let mut events = vec![
                BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::MediaLeft { channel_id, public_key: member.clone() },
                },
            ];
            if let Some(ch) = channel {
                if ch.channel_type == ChannelType::Dm {
                    let remaining = channels::get_voice_participants(conn, channel_id)?;
                    if remaining.is_empty() {
                        if let Some((_, other_pk)) = channels::list_dm_channels(conn, member)?
                            .into_iter()
                            .find(|(c, _)| c.id == channel_id)
                        {
                            events.push(BroadcastEvent {
                                target: EventTarget::Members(vec![other_pk]),
                                event: ServerEvent::StreamCallEnded { channel_id },
                            });
                        }
                    }
                }
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::GetMediaState { channel_id } => {
            let participants = channels::get_voice_participants(conn, channel_id)?;
            let mut voice_members = Vec::new();
            for (pk, joined_at) in participants {
                let name = members::get_member(conn, &pk)?
                    .map(|m| m.display_name).unwrap_or_default();
                voice_members.push(VoiceMember { public_key: pk, display_name: name, joined_at });
            }
            ok(ServerResponse::MediaStateResp { participants: voice_members })
        }

        ServerRequest::JoinStream { channel_id } => {
            use crate::media_stream::ServerSession;
            let member_bytes = *member.as_bytes();
            let display_name = members::get_member(conn, member)?
                .map(|m| m.display_name).unwrap_or_default();
            let mut channels_map = state.media.channels.write().unwrap();
            let stream_state = channels_map.entry(channel_id).or_default();
            let session_id = stream_state.allocate_session_id();
            stream_state.sessions.insert(session_id, ServerSession {
                connection_pk: member_bytes,
                channel_id,
                public_key: member.clone(),
                display_name: display_name.clone(),
                active_tracks: std::collections::HashSet::new(),
                buckets: std::collections::HashMap::new(),
                last_audio_frame_ms: None,
                last_video_frame_ms: None,
                last_screen_audio_frame_ms: None,
            });
            drop(channels_map);

            let events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::StreamJoined {
                    channel_id,
                    public_key: member.clone(),
                    display_name,
                    session_id,
                    active_tracks: vec![],
                    muted: false,
                    deafened: false,
                },
            }];
            ok_with(ServerResponse::StreamSessionStarted { session_id }, events)
        }

        ServerRequest::LeaveStream => {
            let member_bytes = *member.as_bytes();
            let mut channels_map = state.media.channels.write().unwrap();
            let mut events: Vec<BroadcastEvent> = Vec::new();
            for (ch_id, stream_state) in channels_map.iter_mut() {
                let to_remove: Vec<_> = stream_state.sessions.iter()
                    .filter(|(_, s)| s.connection_pk == member_bytes)
                    .map(|(sid, _)| *sid).collect();
                for sid in to_remove {
                    stream_state.sessions.remove(&sid);
                    stream_state.deafened.remove(&sid);
                    stream_state.muted.remove(&sid);
                    events.push(BroadcastEvent {
                        target: EventTarget::All,
                        event: ServerEvent::StreamLeft { channel_id: *ch_id, session_id: sid },
                    });
                }
            }
            drop(channels_map);
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::EnableTrack { kind } => {
            let member_bytes = *member.as_bytes();
            let mut channels_map = state.media.channels.write().unwrap();
            let mut events: Vec<BroadcastEvent> = Vec::new();
            for (ch_id, stream_state) in channels_map.iter_mut() {
                for (sid, session) in stream_state.sessions.iter_mut() {
                    if session.connection_pk == member_bytes {
                        session.active_tracks.insert(kind);
                        events.push(BroadcastEvent {
                            target: EventTarget::All,
                            event: ServerEvent::TrackEnabled {
                                channel_id: *ch_id, session_id: *sid, kind,
                            },
                        });
                    }
                }
            }
            drop(channels_map);
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::DisableTrack { kind } => {
            let member_bytes = *member.as_bytes();
            let mut channels_map = state.media.channels.write().unwrap();
            let mut events: Vec<BroadcastEvent> = Vec::new();
            for (ch_id, stream_state) in channels_map.iter_mut() {
                for (sid, session) in stream_state.sessions.iter_mut() {
                    if session.connection_pk == member_bytes {
                        session.active_tracks.remove(&kind);
                        events.push(BroadcastEvent {
                            target: EventTarget::All,
                            event: ServerEvent::TrackDisabled {
                                channel_id: *ch_id, session_id: *sid, kind,
                            },
                        });
                    }
                }
            }
            drop(channels_map);
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::SetDeafen { deafened } => {
            let member_bytes = *member.as_bytes();
            let mut channels_map = state.media.channels.write().unwrap();
            let mut events: Vec<BroadcastEvent> = Vec::new();
            for (ch_id, stream_state) in channels_map.iter_mut() {
                let session_ids: Vec<_> = stream_state.sessions.iter()
                    .filter(|(_, s)| s.connection_pk == member_bytes)
                    .map(|(sid, _)| *sid).collect();
                for sid in session_ids {
                    if deafened { stream_state.deafened.insert(sid); }
                    else { stream_state.deafened.remove(&sid); }
                    let muted = stream_state.muted.contains(&sid);
                    events.push(BroadcastEvent {
                        target: EventTarget::All,
                        event: ServerEvent::StreamStateChanged {
                            channel_id: *ch_id, session_id: sid, muted, deafened,
                        },
                    });
                }
            }
            drop(channels_map);
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::SetMute { muted } => {
            let member_bytes = *member.as_bytes();
            let mut channels_map = state.media.channels.write().unwrap();
            let mut events: Vec<BroadcastEvent> = Vec::new();
            for (ch_id, stream_state) in channels_map.iter_mut() {
                let session_ids: Vec<_> = stream_state.sessions.iter()
                    .filter(|(_, s)| s.connection_pk == member_bytes)
                    .map(|(sid, _)| *sid).collect();
                for sid in session_ids {
                    if muted { stream_state.muted.insert(sid); }
                    else { stream_state.muted.remove(&sid); }
                    let deafened = stream_state.deafened.contains(&sid);
                    events.push(BroadcastEvent {
                        target: EventTarget::All,
                        event: ServerEvent::StreamStateChanged {
                            channel_id: *ch_id, session_id: sid, muted, deafened,
                        },
                    });
                }
            }
            drop(channels_map);
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::OfferStreamKey { kind, wrapped_keys } => {
            let member_bytes = *member.as_bytes();
            let channels_map = state.media.channels.read().unwrap();
            let mut sender_info: Option<(u64, [u8; 16])> = None;
            for (ch_id, stream_state) in channels_map.iter() {
                if let Some((sid, _)) = stream_state.sessions.iter()
                    .find(|(_, s)| s.connection_pk == member_bytes)
                {
                    sender_info = Some((*ch_id, *sid));
                    break;
                }
            }
            drop(channels_map);
            let (channel_id, session_id) = match sender_info {
                Some(x) => x,
                None => return err("must call JoinStream before OfferStreamKey"),
            };

            let events: Vec<BroadcastEvent> = wrapped_keys.into_iter()
                .map(|(peer_pk, wrapped_key)| BroadcastEvent {
                    target: EventTarget::Members(vec![peer_pk]),
                    event: ServerEvent::StreamKeyOffer {
                        channel_id,
                        sender: member.clone(),
                        session_id,
                        kind,
                        wrapped_key,
                    },
                })
                .collect();
            ok_with(ServerResponse::Ok, events)
        }

        // ----------------------------------------------------------------
        // Profile sync (storage wired in Task 3/4)
        // ----------------------------------------------------------------
        ServerRequest::UpdateProfile { profile } => {
            // 2.5 MB ceiling on the whole signed blob (avatar cap is 2 MB inside).
            if profile.len() > 2_621_440 {
                return err("profile too large (max 2.5 MB)");
            }
            let signed = match farder_crypto::profile::SignedProfile::from_bytes(&profile) {
                Ok(p) => p,
                Err(_) => return err("malformed profile"),
            };
            if &signed.data.public_key != member {
                return err("profile public key does not match authenticated member");
            }
            if signed.verify().is_err() {
                return err("profile signature invalid");
            }
            if signed.data.display_name.chars().count() > 128 {
                return err("display name too long (max 128 characters)");
            }
            if let Some(status) = &signed.data.status {
                if status.chars().count() > 128 {
                    return err("status too long (max 128 characters)");
                }
            }
            if let Some(avatar) = &signed.data.avatar {
                if let Err(e) = crate::image_validation::validate_image(avatar, true) {
                    return err(&format!("avatar rejected: {}", e));
                }
            }

            let hash = crate::attachments::compute_sha256(&profile);
            members::set_member_profile(conn, member, &signed.data.display_name, &profile, &hash)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::MemberProfileUpdated {
                        public_key: member.clone(),
                        profile_hash: Some(hash),
                    },
                }],
            )
        }

        ServerRequest::GetMemberProfile { member_key } => {
            let profile = members::get_member_profile(conn, &member_key)?;
            ok(ServerResponse::MemberProfile { member_key, profile })
        }

        // ----------------------------------------------------------------
        // Rich presence
        // ----------------------------------------------------------------
        ServerRequest::UpdatePresence { presence } => {
            let pk_bytes = *member.as_bytes();
            // Rate-limit: silently return Ok if the user is over the cap.
            if !state.presence_limiter.allow(&pk_bytes) {
                tracing::debug!(member = %member, "presence update rate-limited (dropped)");
                return ok(ServerResponse::Ok);
            }
            // Validate field lengths.
            if let Some(p) = &presence {
                if p.details.chars().count() > 128 {
                    return err("presence details too long (max 128 characters)");
                }
                if p.state.as_ref().is_some_and(|s| s.chars().count() > 128) {
                    return err("presence state too long (max 128 characters)");
                }
            }
            {
                let mut map = state.presences.write().unwrap();
                match &presence {
                    Some(p) => { map.insert(pk_bytes, p.clone()); }
                    None => { map.remove(&pk_bytes); }
                }
            }
            tracing::info!(member = %member, presence = ?presence, "presence updated; broadcasting");
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::MemberPresenceUpdated {
                        public_key: member.clone(),
                        presence,
                    },
                }],
            )
        }
        ServerRequest::SubmitEvent { event } => {
            // 1. The server must be in log mode (genesis established).
            let mut ls_guard = state.log_state.lock().unwrap();
            let ls = match ls_guard.as_ref() {
                Some(ls) => ls,
                None => return err("server is not running the event log (no genesis yet)"),
            };

            // 2. Validate on a CLONE — apply runs the full envelope + authz; on
            //    error nothing is mutated and we reject.
            let mut trial = ls.clone();
            if let Err(e) = trial.apply(&event) {
                return err(&format!("event rejected: {}", e));
            }

            // 3. For a message event, the referenced channel must exist (channels
            //    are still legacy DB state this slice).
            if let EventPayload::MessagePosted { channel_id, content, .. } = &event.core.payload {
                if content.len() > 8000 {
                    return err("message content too long (max 8000 characters)");
                }
                if channels::get_channel(conn, *channel_id)?.is_none() {
                    return err("channel not found");
                }
            }

            // 4. Persist the event (source of truth) + derive the message row + its
            //    validated attachments — atomically, so a crash never leaves a message
            //    without its attachment rows.
            let owner_pk = state.genesis.lock().unwrap().as_ref().map(|g| g.owner.clone());

            // Compute by_moderator from `trial` BEFORE it is moved into ls_guard.
            let redaction_info: Option<(String, bool)> = if let EventPayload::AttachmentRedacted { content_hash } = &event.core.payload {
                let by_moderator = trial
                    .attachment_uploader(content_hash)
                    .map(|up| up != &event.core.author)
                    .unwrap_or(true);
                Some((content_hash.clone(), by_moderator))
            } else {
                None
            };

            let derived_id = {
                let tx = conn.unchecked_transaction()
                    .map_err(|e| anyhow::anyhow!("failed to begin tx: {}", e))?;
                crate::event_ingest::store_event(&tx, &event)
                    .map_err(|e| anyhow::anyhow!("failed to store event: {}", e))?;
                let id = crate::event_ingest::derive_message_row(&tx, &event)
                    .map_err(|e| anyhow::anyhow!("failed to derive message: {}", e))?;
                if let (Some(mid), Some(owner)) = (id, owner_pk.as_ref()) {
                    crate::event_ingest::derive_attachments(&tx, mid, &event, owner)
                        .map_err(|e| anyhow::anyhow!("failed to derive attachments: {}", e))?;
                }
                if let EventPayload::AttachmentRedacted { content_hash } = &event.core.payload {
                    crate::attachments::redact_blob(&tx, &state.storage_dir, content_hash, &event.core.author)
                        .map_err(|e| anyhow::anyhow!("failed to redact attachment: {}", e))?;
                }
                tx.commit().map_err(|e| anyhow::anyhow!("failed to commit event: {}", e))?;
                id
            };

            // 5. Commit the advanced authorization state in memory.
            let timestamp = event.core.timestamp;
            *ls_guard = Some(trial);
            drop(ls_guard);

            // 6. Broadcast: for a derived message, send NewMessage so the existing
            //    client render path works unchanged.
            let mut events = Vec::new();
            if let Some(mid) = derived_id {
                if let EventPayload::MessagePosted { channel_id, .. } = &event.core.payload {
                    if let Some(msg) = messages::get_message(conn, mid, member)? {
                        events.push(BroadcastEvent {
                            target: EventTarget::Subscribers(*channel_id),
                            event: ServerEvent::NewMessage { message: msg },
                        });
                    }
                }
            }

            // Broadcast a membership change so every client re-fetches its own
            // status + the member list + the pending queue.
            let membership_pk: Option<PublicKey> = match &event.core.payload {
                EventPayload::MemberJoined { member, .. }
                | EventPayload::MemberApproved { member }
                | EventPayload::MemberRemoved { member }
                | EventPayload::MemberBanned { member } => Some(member.clone()),
                _ => None,
            };
            if let Some(pk) = membership_pk {
                events.push(BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::MembershipChanged { public_key: pk },
                });
            }

            // Broadcast redaction so connected clients flip the placeholder.
            if let Some((content_hash, by_moderator)) = redaction_info {
                events.push(BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::AttachmentRedacted { content_hash, by_moderator },
                });
            }

            ok_with(ServerResponse::EventAccepted { event_hash: event.hash(), timestamp }, events)
        }

        ServerRequest::AddBot { coin_id, label } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            let coin_id = coin_id.trim().to_lowercase();
            if coin_id.is_empty()
                || coin_id.len() > 64
                || !coin_id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                return err("invalid coin id (use a CoinGecko id: lowercase letters, digits, hyphens)");
            }
            let label = label.trim().to_string();
            if label.is_empty() || label.len() > 64 {
                return err("bot label must be 1-64 characters");
            }
            let kp = farder_crypto::identity::Keypair::generate();
            let pk = kp.public_key();
            members::register_bot_member(conn, &pk, &label)?;
            crate::bots::register_bot(conn, &pk, kp.signing_key_bytes().as_slice(), &coin_id, &label)?;
            ok_with(ServerResponse::Ok, vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberJoined { public_key: pk, display_name: label },
            }])
        }

        ServerRequest::RemoveBot { bot_public_key } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            crate::bots::remove_bot(conn, &bot_public_key)?;
            members::remove_member_row(conn, &bot_public_key)?;
            {
                let mut map = state.presences.write().unwrap();
                map.remove(bot_public_key.as_bytes());
            }
            ok_with(ServerResponse::Ok, vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberLeft { public_key: bot_public_key },
            }])
        }

        ServerRequest::SetBotPollInterval { secs } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            crate::bots::set_poll_interval(conn, secs)?;
            ok(ServerResponse::Ok)
        }

        ServerRequest::GetBotPollInterval => {
            ok(ServerResponse::BotPollInterval { secs: crate::bots::get_poll_interval(conn) })
        }

        ServerRequest::AddBotAlert { bot_public_key, metric, comparator, threshold } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            if metric != "price_usd" && metric != "change_24h" && metric != "value" {
                return err("invalid metric: must be 'price_usd', 'change_24h', or 'value'");
            }
            if comparator != "above" && comparator != "below" {
                return err("invalid comparator: must be 'above' or 'below'");
            }
            let _id = crate::bots::add_alert(conn, &bot_public_key, &metric, &comparator, threshold)?;
            ok(ServerResponse::Ok)
        }

        ServerRequest::RemoveBotAlert { alert_id } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            crate::bots::remove_alert(conn, alert_id)?;
            ok(ServerResponse::Ok)
        }

        ServerRequest::ListBotAlerts { bot_public_key } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            let rows = crate::bots::list_alerts_for_bot(conn, &bot_public_key)?;
            let alerts = rows.into_iter().map(|a| BotAlertInfo {
                id: a.id,
                metric: a.metric,
                comparator: a.comparator,
                threshold: a.threshold,
            }).collect();
            ok(ServerResponse::BotAlerts { alerts })
        }

        // SubscribeBot/UnsubscribeBot/ListMySubscriptions: any member, no permission gate.
        // The subscriber is always the authenticated `member` — never a client-supplied key.
        ServerRequest::SubscribeBot { bot_public_key } => {
            crate::bots::subscribe(conn, &bot_public_key, member)?;
            ok(ServerResponse::Ok)
        }

        ServerRequest::UnsubscribeBot { bot_public_key } => {
            crate::bots::unsubscribe(conn, &bot_public_key, member)?;
            ok(ServerResponse::Ok)
        }

        ServerRequest::ListMySubscriptions => {
            let bot_public_keys = crate::bots::list_subscriptions_for_user(conn, member)?;
            ok(ServerResponse::MySubscriptions { bot_public_keys })
        }

        // ----------------------------------------------------------------
        // Webhook management (MANAGE_SERVER gated)
        // ----------------------------------------------------------------
        ServerRequest::CreateWebhook { channel_id, name } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            if channels::get_channel(conn, channel_id)?.is_none() {
                return err("channel not found");
            }
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 64 {
                return err("webhook name must be 1-64 characters");
            }
            let (id, token) = crate::webhooks::create(conn, channel_id, &name)?;
            let server_id_hex = state.relay_server_id.lock().unwrap().clone();
            ok(ServerResponse::WebhookToken { id, token, server_id_hex })
        }

        ServerRequest::RegenerateWebhookToken { id } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            match crate::webhooks::regenerate_token(conn, id)? {
                Some(token) => {
                    let server_id_hex = state.relay_server_id.lock().unwrap().clone();
                    ok(ServerResponse::WebhookToken { id, token, server_id_hex })
                }
                None => err("webhook not found"),
            }
        }

        ServerRequest::DeleteWebhook { id } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            crate::webhooks::delete(conn, id)?;
            ok(ServerResponse::Ok)
        }

        ServerRequest::ListWebhooks { channel_id } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            let rows = crate::webhooks::list_for_channel(conn, channel_id)?;
            let webhooks = rows.into_iter().map(|r| WebhookInfo {
                id: r.id,
                channel_id: r.channel_id,
                name: r.name,
            }).collect();
            ok(ServerResponse::Webhooks { webhooks })
        }

        ServerRequest::AddCustomBot { name, source_url, value_path, unit } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            let name = name.trim().to_string();
            let source_url = source_url.trim().to_string();
            let value_path = value_path.trim().to_string();
            if name.is_empty() || name.len() > 64 { return err("bot name must be 1-64 characters"); }
            if !(source_url.starts_with("http://") || source_url.starts_with("https://")) || source_url.len() > 2048 {
                return err("source url must be http(s) and <= 2048 chars");
            }
            if value_path.is_empty() || value_path.len() > 256 { return err("value path required (<=256 chars)"); }
            let unit = unit.map(|u| u.trim().chars().take(24).collect::<String>()).filter(|u| !u.is_empty());
            let kp = farder_crypto::identity::Keypair::generate();
            let pk = kp.public_key();
            crate::members::register_bot_member(conn, &pk, &name)?;
            crate::bots::register_custom_bot(conn, &pk, kp.signing_key_bytes().as_slice(), &name, &source_url, &value_path, unit.as_deref())?;
            ok_with(ServerResponse::Ok, vec![BroadcastEvent { target: EventTarget::All, event: ServerEvent::MemberJoined { public_key: pk, display_name: name } }])
        }

        // ----------------------------------------------------------------
        // Slash commands
        // ----------------------------------------------------------------
        ServerRequest::ListCommands {} => {
            ok(ServerResponse::Commands { commands: crate::commands::list_infos(conn)? })
        }

        ServerRequest::DeleteCommand { id } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            crate::commands::delete(conn, id)?;
            ok(ServerResponse::Ok)
        }

        // RunCommand is async (may fetch a remote JSON API) and must be handled at the
        // connection level (like FetchUrl). This arm is here solely for exhaustive match.
        ServerRequest::RunCommand { .. } => {
            err("RunCommand must be handled at the connection level")
        }

        ServerRequest::AddCommand { name, trigger, description, kind, body_text, url_template, value_path, response_template, unit } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            let name = name.trim().to_string();
            let trigger = trigger.trim().to_lowercase();
            if name.is_empty() || name.len() > 48 { return err("command name must be 1-48 chars"); }
            if trigger.is_empty() || trigger.len() > 32 || !trigger.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                return err("trigger must be 1-32 chars of a-z 0-9 _ -");
            }
            if description.len() > 160 { return err("description too long"); }
            if crate::commands::find_by_trigger(conn, &trigger)?.is_some() { return err("a command with that trigger already exists"); }
            match kind.as_str() {
                "text" => {
                    if body_text.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true) {
                        return err("text command needs body text");
                    }
                }
                "api" => {
                    let u = url_template.as_deref().unwrap_or("");
                    if !(u.starts_with("http://") || u.starts_with("https://")) || u.len() > 2048 {
                        return err("api command needs an http(s) url template (<=2048)");
                    }
                    if value_path.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true) {
                        return err("api command needs a value path");
                    }
                }
                _ => return err("kind must be 'text' or 'api'"),
            }
            crate::commands::create(
                conn,
                &name,
                &trigger,
                description.trim(),
                &kind,
                body_text.as_deref(),
                url_template.as_deref(),
                value_path.as_deref(),
                response_template.as_deref(),
                unit.as_deref(),
            )?;
            ok(ServerResponse::Ok)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use farder_crypto::identity::Keypair;

    fn setup() -> (Connection, PublicKey) {
        let conn = db::open_in_memory().unwrap();
        let everyone_id = members::create_role(
            &conn,
            "@everyone",
            permissions::DEFAULT_EVERYONE,
            None,
            0,
            true,
            false,
        )
        .unwrap();
        let owner_kp = Keypair::generate();
        members::register_member(&conn, &owner_kp.public_key(), "Owner").unwrap();
        members::assign_role(&conn, &owner_kp.public_key(), everyone_id).unwrap();
        (conn, owner_kp.public_key())
    }

    fn add_member(conn: &Connection, name: &str) -> PublicKey {
        let kp = Keypair::generate();
        members::register_member(conn, &kp.public_key(), name).unwrap();
        let everyone_id: u64 = conn
            .query_row(
                "SELECT id FROM roles WHERE name = '@everyone'",
                [],
                |row| Ok(row.get::<_, i64>(0)? as u64),
            )
            .unwrap();
        members::assign_role(conn, &kp.public_key(), everyone_id).unwrap();
        kp.public_key()
    }

    fn make_channel(conn: &Connection) -> u64 {
        channels::create_channel(conn, "general", ChannelType::Text, None, 0).unwrap()
    }

    fn fake_state() -> Arc<ServerState> {
        Arc::new(ServerState::new_for_test().unwrap())
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_handle_send_message() {
        let (conn, owner_pk) = setup();
        let channel_id = make_channel(&conn);

        let result = handle_request(
            &conn,
            &owner_pk,
            true, // is_owner
            ServerRequest::SendMessage {
                channel_id,
                content: "Hello, world!".to_string(),
                reply_to: None,
                attachment_ids: vec![],
            },
            "",
            &fake_state(),
        )
        .unwrap();

        match result.response {
            ServerResponse::MessageSent { id, .. } => {
                assert!(id > 0, "message id should be > 0");
            }
            other => panic!("expected MessageSent, got {:?}", other),
        }
        assert!(!result.events.is_empty(), "should have broadcast events");
    }

    #[test]
    fn test_handle_send_message_no_permission() {
        let (conn, _owner_pk) = setup();
        let channel_id = make_channel(&conn);

        // Strip SEND_MESSAGES from @everyone so any member without extra roles can't send.
        conn.execute(
            "UPDATE roles SET permissions = ?1 WHERE name = '@everyone' AND builtin = 1",
            rusqlite::params![permissions::VIEW_CHANNEL as i64 | permissions::READ_MESSAGES as i64],
        )
        .unwrap();

        // Create a regular member (only @everyone via add_member).
        let restricted = add_member(&conn, "Restricted");

        let result = handle_request(
            &conn,
            &restricted,
            false,
            ServerRequest::SendMessage {
                channel_id,
                content: "Should fail".to_string(),
                reply_to: None,
                attachment_ids: vec![],
            },
            "",
            &fake_state(),
        )
        .unwrap();

        match result.response {
            ServerResponse::Error { reason } => {
                assert!(
                    reason.contains("SEND_MESSAGES"),
                    "expected SEND_MESSAGES error, got: {}",
                    reason
                );
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_fetch_history() {
        let (conn, owner_pk) = setup();
        let channel_id = make_channel(&conn);

        // Insert 2 messages directly.
        messages::insert_message(&conn, channel_id, &owner_pk, "msg1", None).unwrap();
        messages::insert_message(&conn, channel_id, &owner_pk, "msg2", None).unwrap();

        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::FetchHistory {
                channel_id,
                before_id: None,
                limit: 50,
            },
            "",
            &fake_state(),
        )
        .unwrap();

        match result.response {
            ServerResponse::History { messages: msgs } => {
                assert_eq!(msgs.len(), 2, "should return 2 messages");
            }
            other => panic!("expected History, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_create_channel() {
        let (conn, owner_pk) = setup();

        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::CreateChannel {
                name: "new-channel".to_string(),
                channel_type: ChannelType::Text,
                category_id: None,
                position: Some(0),
            },
            "",
            &fake_state(),
        )
        .unwrap();

        assert!(
            matches!(result.response, ServerResponse::Ok),
            "expected Ok response"
        );
        assert!(!result.events.is_empty(), "should have ChannelCreated event");

        let all = channels::list_channels(&conn).unwrap();
        assert!(
            all.iter().any(|ch| ch.name == "new-channel"),
            "channel should appear in list"
        );
    }

    #[test]
    fn test_handle_create_role() {
        let (conn, owner_pk) = setup();

        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::CreateRole {
                name: "Moderator".to_string(),
                permissions: permissions::MANAGE_MESSAGES,
                color: Some("#FF0000".to_string()),
                position: Some(1),
                hoist: None,
            },
            "",
            &fake_state(),
        )
        .unwrap();

        assert!(
            matches!(result.response, ServerResponse::Ok),
            "expected Ok response"
        );

        let roles = members::list_roles(&conn).unwrap();
        assert!(
            roles.iter().any(|r| r.name == "Moderator"),
            "Moderator role should exist"
        );
    }

    #[test]
    fn test_handle_create_invite() {
        let (conn, owner_pk) = setup();

        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::CreateInvite {
                max_uses: None,
                expires_in_secs: None,
                target_channel: None,
            },
            "",
            &fake_state(),
        )
        .unwrap();

        match result.response {
            ServerResponse::InviteCreated { code } => {
                assert_eq!(code.len(), 8, "invite code should be 8 chars");
            }
            other => panic!("expected InviteCreated, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_get_server_info() {
        let (conn, owner_pk) = setup();
        let _channel_id = make_channel(&conn);

        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::GetServerInfo,
            "",
            &fake_state(),
        )
        .unwrap();

        match result.response {
            ServerResponse::ServerInfo {
                member_count,
                channels,
                roles,
                ..
            } => {
                assert!(member_count >= 1, "at least 1 member (owner)");
                assert!(!channels.is_empty(), "should have at least 1 channel");
                assert!(!roles.is_empty(), "should have at least @everyone role");
            }
            other => panic!("expected ServerInfo, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_edit_own_message() {
        let (conn, owner_pk) = setup();
        let channel_id = make_channel(&conn);

        // Send a message first.
        let send_result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::SendMessage {
                channel_id,
                content: "Original content".to_string(),
                reply_to: None,
                attachment_ids: vec![],
            },
            "",
            &fake_state(),
        )
        .unwrap();

        let msg_id = match send_result.response {
            ServerResponse::MessageSent { id, .. } => id,
            other => panic!("expected MessageSent, got {:?}", other),
        };

        // Edit the message.
        let edit_result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::EditMessage {
                message_id: msg_id,
                new_content: "Edited content".to_string(),
            },
            "",
            &fake_state(),
        )
        .unwrap();

        assert!(
            matches!(edit_result.response, ServerResponse::Ok),
            "expected Ok"
        );

        // Verify content changed.
        let msg = messages::get_message(&conn, msg_id, &owner_pk).unwrap().unwrap();
        assert_eq!(msg.content, "Edited content");
    }

    #[test]
    fn test_handle_search() {
        let (conn, owner_pk) = setup();
        let channel_id = make_channel(&conn);

        messages::insert_message(&conn, channel_id, &owner_pk, "hello world", None).unwrap();
        messages::insert_message(&conn, channel_id, &owner_pk, "goodbye world", None).unwrap();

        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::Search {
                query: "hello".to_string(),
                channel_id: Some(channel_id),
                limit: 50,
            },
            "",
            &fake_state(),
        )
        .unwrap();

        match result.response {
            ServerResponse::SearchResults { messages: msgs } => {
                assert_eq!(msgs.len(), 1, "should find 1 message matching 'hello'");
                assert!(msgs[0].content.contains("hello"));
            }
            other => panic!("expected SearchResults, got {:?}", other),
        }
    }

    #[test]
    fn test_timed_out_send_message_rejected() {
        let (conn, _owner_pk) = setup();
        let actor = add_member(&conn, "Talker");
        let channel_id = make_channel(&conn);

        // Time out the actor for 60 seconds.
        members::set_timeout(&conn, &actor, current_unix_ms() + 60_000, Some("spam")).unwrap();

        let result = handle_request(
            &conn,
            &actor,
            false,
            ServerRequest::SendMessage {
                channel_id,
                content: "hi".to_string(),
                reply_to: None,
                attachment_ids: vec![],
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();

        match result.response {
            ServerResponse::Error { reason } => {
                assert!(reason.contains("timed out"), "got: {reason}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn test_handle_ban_member() {
        let (conn, owner_pk) = setup();
        let victim = add_member(&conn, "Victim");

        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::BanMember {
                member_key: victim.clone(),
                reason: None,
            },
            "",
            &fake_state(),
        )
        .unwrap();

        assert!(
            matches!(result.response, ServerResponse::Ok),
            "expected Ok"
        );

        // Verify banned flag is set.
        let rec = members::get_member(&conn, &victim).unwrap().unwrap();
        assert!(rec.banned, "member should be banned");
    }

    #[test]
    fn test_cannot_create_role_above_own_position() {
        let (conn, _owner) = setup();
        // Create a mod role at position 2
        let mod_role_id = members::create_role(&conn, "Mod", permissions::MANAGE_ROLES | permissions::DEFAULT_EVERYONE, None, 2, false, false).unwrap();
        let moderator = add_member(&conn, "Moderator");
        members::assign_role(&conn, &moderator, mod_role_id).unwrap();

        // Mod tries to create a role at position 3 (above their position 2) — should fail
        let result = handle_request(&conn, &moderator, false, ServerRequest::CreateRole {
            name: "SuperAdmin".to_string(),
            permissions: permissions::ALL_PERMISSIONS,
            color: None,
            position: Some(3),
            hoist: None,
        }, "", &fake_state()).unwrap();
        match result.response {
            ServerResponse::Error { .. } => {}
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn test_cannot_kick_member_with_higher_role() {
        let (conn, _owner) = setup();
        // Create admin role at position 3, mod role at position 2
        let admin_role = members::create_role(&conn, "Admin", permissions::ALL_PERMISSIONS, None, 3, false, false).unwrap();
        let mod_role = members::create_role(&conn, "Mod", permissions::KICK_MEMBERS | permissions::DEFAULT_EVERYONE, None, 2, false, false).unwrap();

        let admin = add_member(&conn, "Admin");
        members::assign_role(&conn, &admin, admin_role).unwrap();

        let moderator = add_member(&conn, "Moderator");
        members::assign_role(&conn, &moderator, mod_role).unwrap();

        // Mod tries to kick Admin — should fail (admin position 3 > mod position 2)
        let result = handle_request(&conn, &moderator, false, ServerRequest::KickMember {
            member_key: admin.clone(),
        }, "", &fake_state()).unwrap();
        match result.response {
            ServerResponse::Error { .. } => {}
            other => panic!("expected Error, got {:?}", other),
        }

        // Mod can kick a regular member (position 0, below mod's 2)
        let regular = add_member(&conn, "Regular");
        let result = handle_request(&conn, &moderator, false, ServerRequest::KickMember {
            member_key: regular.clone(),
        }, "", &fake_state()).unwrap();
        match result.response {
            ServerResponse::Ok => {}
            other => panic!("expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_owner_bypasses_hierarchy() {
        let (conn, owner) = setup();
        // Owner can create roles at any position
        let result = handle_request(&conn, &owner, true, ServerRequest::CreateRole {
            name: "HighRole".to_string(),
            permissions: 0xFF,
            color: None,
            position: Some(999),
            hoist: None,
        }, "", &fake_state()).unwrap();
        match result.response {
            ServerResponse::Ok => {}
            other => panic!("expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_send_message_with_attachments() {
        let (conn, owner) = setup();
        let ch_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let dir = std::env::temp_dir().join(format!("farder-handler-test-{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let data = b"image bytes here";
        let hash = crate::attachments::compute_sha256(data);
        let file_id = crate::attachments::store_file(
            &conn, &dir.to_string_lossy(), &owner, "pic.png", data, &hash, "application/octet-stream", None, None, None
        ).unwrap();

        let result = handle_request(&conn, &owner, true, ServerRequest::SendMessage {
            channel_id: ch_id,
            content: "check this".to_string(),
            reply_to: None,
            attachment_ids: vec![file_id],
        }, "", &fake_state()).unwrap();
        match result.response {
            ServerResponse::MessageSent { id, .. } => {
                let msg = messages::get_message(&conn, id, &owner).unwrap().unwrap();
                assert_eq!(msg.attachments.len(), 1);
                assert_eq!(msg.attachments[0].name, "pic.png");
                let file = crate::attachments::get_file(&conn, file_id).unwrap().unwrap();
                assert_eq!(file.ref_count, 1);
            }
            other => panic!("expected MessageSent, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_handle_send_message_too_many_attachments() {
        let (conn, owner) = setup();
        let ch_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let result = handle_request(&conn, &owner, true, ServerRequest::SendMessage {
            channel_id: ch_id,
            content: "too many".to_string(),
            reply_to: None,
            attachment_ids: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        }, "", &fake_state()).unwrap();
        match result.response {
            ServerResponse::Error { .. } => {}
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_create_thread() {
        let (conn, owner) = setup();
        let ch_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let msg_id = messages::insert_message(&conn, ch_id, &owner, "thread me", None).unwrap();
        let result = handle_request(&conn, &owner, true, ServerRequest::CreateThread {
            message_id: msg_id, name: Some("discussion".to_string()),
        }, "", &fake_state()).unwrap();
        match result.response { ServerResponse::Ok => {} other => panic!("expected Ok, got {:?}", other) }
        assert!(!result.events.is_empty());
    }

    #[test]
    fn test_handle_add_reaction() {
        let (conn, owner) = setup();
        let ch_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let msg_id = messages::insert_message(&conn, ch_id, &owner, "react", None).unwrap();
        let result = handle_request(&conn, &owner, true, ServerRequest::AddReaction {
            message_id: msg_id, emoji: "👍".to_string(), file_id: None,
        }, "", &fake_state()).unwrap();
        match result.response { ServerResponse::Ok => {} other => panic!("expected Ok, got {:?}", other) }
        let msg = messages::get_message(&conn, msg_id, &owner).unwrap().unwrap();
        assert_eq!(msg.reactions.len(), 1);
    }

    #[test]
    fn test_handle_remove_reaction() {
        let (conn, owner) = setup();
        let ch_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let msg_id = messages::insert_message(&conn, ch_id, &owner, "react", None).unwrap();
        crate::reactions::add_reaction(&conn, msg_id, &owner, "👍", None).unwrap();
        let result = handle_request(&conn, &owner, true, ServerRequest::RemoveReaction {
            message_id: msg_id, emoji: "👍".to_string(), file_id: None,
        }, "", &fake_state()).unwrap();
        match result.response { ServerResponse::Ok => {} other => panic!("expected Ok, got {:?}", other) }
    }

    #[test]
    fn test_handle_request_deletion() {
        let (conn, _owner_pk) = setup();
        let member = add_member(&conn, "Alice");

        let result = handle_request(&conn, &member, false, ServerRequest::RequestDeletion, "", &fake_state()).unwrap();
        match result.response {
            ServerResponse::Ok => {}
            other => panic!("expected Ok, got {:?}", other),
        }
        assert_eq!(result.events.len(), 1, "should broadcast DeletionRequested event");
        match &result.events[0].event {
            ServerEvent::DeletionRequested { public_key } => assert_eq!(public_key, &member),
            other => panic!("expected DeletionRequested event, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_request_deletion_owner_rejected() {
        let (conn, owner_pk) = setup();

        let result = handle_request(&conn, &owner_pk, true, ServerRequest::RequestDeletion, "", &fake_state()).unwrap();
        match result.response {
            ServerResponse::Error { reason } => {
                assert!(reason.contains("owner"), "error should mention owner: {}", reason);
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_cancel_deletion() {
        let (conn, _owner_pk) = setup();
        let member = add_member(&conn, "Bob");

        // Request deletion first
        handle_request(&conn, &member, false, ServerRequest::RequestDeletion, "", &fake_state()).unwrap();

        // Cancel deletion
        let result = handle_request(&conn, &member, false, ServerRequest::CancelDeletion, "", &fake_state()).unwrap();
        match result.response {
            ServerResponse::Ok => {}
            other => panic!("expected Ok for CancelDeletion, got {:?}", other),
        }
        assert_eq!(result.events.len(), 1, "should broadcast DeletionCancelled event");
        match &result.events[0].event {
            ServerEvent::DeletionCancelled { public_key } => assert_eq!(public_key, &member),
            other => panic!("expected DeletionCancelled event, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_get_deletion_status() {
        let (conn, _owner_pk) = setup();
        let member = add_member(&conn, "Carol");

        // Before requesting deletion: not pending
        let result = handle_request(&conn, &member, false, ServerRequest::GetDeletionStatus, "", &fake_state()).unwrap();
        match result.response {
            ServerResponse::DeletionStatusResp { status } => {
                assert!(!status.pending);
                assert!(status.requested_at.is_none());
                assert!(status.expires_at.is_none());
            }
            other => panic!("expected DeletionStatusResp, got {:?}", other),
        }

        // After requesting deletion: pending
        handle_request(&conn, &member, false, ServerRequest::RequestDeletion, "", &fake_state()).unwrap();
        let result = handle_request(&conn, &member, false, ServerRequest::GetDeletionStatus, "", &fake_state()).unwrap();
        match result.response {
            ServerResponse::DeletionStatusResp { status } => {
                assert!(status.pending);
                assert!(status.requested_at.is_some());
                assert!(status.expires_at.is_some());
            }
            other => panic!("expected DeletionStatusResp, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // DM handler tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_handle_open_dm() {
        let (conn, owner_pk) = setup();
        let alice = add_member(&conn, "Alice");

        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::OpenDm { target_key: alice.clone() },
            "",
            &fake_state(),
        )
        .unwrap();

        match result.response {
            ServerResponse::DmOpened { channel, participant } => {
                assert_eq!(channel.channel_type, ChannelType::Dm);
                assert_eq!(participant.public_key, alice);
            }
            other => panic!("expected DmOpened, got {:?}", other),
        }
        // DmCreated event should be emitted.
        assert_eq!(result.events.len(), 1);
        match &result.events[0].event {
            ServerEvent::DmCreated { .. } => {}
            other => panic!("expected DmCreated event, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_open_dm_idempotent() {
        let (conn, owner_pk) = setup();
        let alice = add_member(&conn, "Alice");

        let r1 = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::OpenDm { target_key: alice.clone() },
            "",
            &fake_state(),
        )
        .unwrap();
        let r2 = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::OpenDm { target_key: alice.clone() },
            "",
            &fake_state(),
        )
        .unwrap();

        let ch1 = match r1.response {
            ServerResponse::DmOpened { channel, .. } => channel.id,
            other => panic!("expected DmOpened, got {:?}", other),
        };
        let ch2 = match r2.response {
            ServerResponse::DmOpened { channel, .. } => channel.id,
            other => panic!("expected DmOpened, got {:?}", other),
        };
        assert_eq!(ch1, ch2, "should return the same channel");
        // Second open should not emit DmCreated.
        assert!(r2.events.is_empty(), "no events on second open");
    }

    #[test]
    fn test_handle_list_dms() {
        let (conn, owner_pk) = setup();
        let alice = add_member(&conn, "Alice");
        let bob = add_member(&conn, "Bob");

        // Open two DMs.
        handle_request(&conn, &owner_pk, true, ServerRequest::OpenDm { target_key: alice }, "", &fake_state()).unwrap();
        handle_request(&conn, &owner_pk, true, ServerRequest::OpenDm { target_key: bob }, "", &fake_state()).unwrap();

        let result = handle_request(&conn, &owner_pk, true, ServerRequest::ListDms, "", &fake_state()).unwrap();
        match result.response {
            ServerResponse::DmList { dms } => {
                assert_eq!(dms.len(), 2);
            }
            other => panic!("expected DmList, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_block_prevents_dm() {
        let (conn, owner_pk) = setup();
        let alice = add_member(&conn, "Alice");

        // First open the DM channel.
        let open_result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::OpenDm { target_key: alice.clone() },
            "",
            &fake_state(),
        )
        .unwrap();
        let dm_channel_id = match open_result.response {
            ServerResponse::DmOpened { channel, .. } => channel.id,
            other => panic!("expected DmOpened, got {:?}", other),
        };

        // Block alice.
        handle_request(&conn, &owner_pk, true, ServerRequest::BlockUser { target_key: alice.clone() }, "", &fake_state()).unwrap();

        // Sending a message in that DM should now fail.
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::SendMessage {
                channel_id: dm_channel_id,
                content: "hello".to_string(),
                reply_to: None,
                attachment_ids: vec![],
            },
            "",
            &fake_state(),
        )
        .unwrap();
        match result.response {
            ServerResponse::Error { reason } => {
                assert!(reason.contains("blocked"), "expected blocked error, got: {}", reason);
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn test_send_message_in_dm() {
        let (conn, owner_pk) = setup();
        let alice = add_member(&conn, "Alice");

        // Open DM.
        let open_result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::OpenDm { target_key: alice.clone() },
            "",
            &fake_state(),
        )
        .unwrap();
        let dm_channel_id = match open_result.response {
            ServerResponse::DmOpened { channel, .. } => channel.id,
            other => panic!("expected DmOpened, got {:?}", other),
        };

        // Owner sends a message — should succeed without permission checks.
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::SendMessage {
                channel_id: dm_channel_id,
                content: "Hey Alice!".to_string(),
                reply_to: None,
                attachment_ids: vec![],
            },
            "",
            &fake_state(),
        )
        .unwrap();
        match result.response {
            ServerResponse::MessageSent { id, .. } => {
                assert!(id > 0);
            }
            other => panic!("expected MessageSent, got {:?}", other),
        }

        // Non-participant should not be able to send.
        let bob = add_member(&conn, "Bob");
        let result2 = handle_request(
            &conn,
            &bob,
            false,
            ServerRequest::SendMessage {
                channel_id: dm_channel_id,
                content: "Sneaky Bob".to_string(),
                reply_to: None,
                attachment_ids: vec![],
            },
            "",
            &fake_state(),
        )
        .unwrap();
        match result2.response {
            ServerResponse::Error { reason } => {
                assert!(reason.contains("participant"), "expected participant error, got: {}", reason);
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn unban_member_clears_ban_and_emits_event() {
        let (conn, owner_pk) = setup();
        let victim = add_member(&conn, "Victim");

        // Ban the victim first.
        let _ = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::BanMember {
                member_key: victim.clone(),
                reason: Some("test".to_string()),
            },
            "",
            &fake_state(),
        )
        .unwrap();

        // Now unban.
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::UnbanMember { member_key: victim.clone() },
            "",
            &fake_state(),
        )
        .unwrap();

        assert!(matches!(result.response, ServerResponse::Ok));
        // Expect MemberUnbanned plus an AuditEventCreated row.
        assert!(
            result
                .events
                .iter()
                .any(|e| matches!(e.event, ServerEvent::MemberUnbanned { .. })),
            "missing MemberUnbanned event"
        );
        assert!(crate::members::list_banned(&conn).unwrap().is_empty());
    }

    #[test]
    fn test_timeout_member_requires_perm() {
        let (conn, _owner_pk) = setup();
        let actor_pk = add_member(&conn, "Actor");
        let target_pk = add_member(&conn, "Target");
        let result = handle_request(
            &conn,
            &actor_pk,
            false,
            ServerRequest::TimeoutMember {
                member_key: target_pk.clone(),
                until_ms: current_unix_ms() + 60_000,
                reason: None,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        match result.response {
            ServerResponse::Error { reason } => {
                assert!(reason.contains("TIMEOUT_MEMBERS"), "got: {reason}")
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn test_timeout_member_rejects_invalid_duration() {
        let (conn, owner_pk) = setup();
        let target_pk = add_member(&conn, "Target");
        let now = current_unix_ms();

        // until_ms in the past.
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::TimeoutMember {
                member_key: target_pk.clone(),
                until_ms: now.saturating_sub(1000),
                reason: None,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(
            matches!(&result.response, ServerResponse::Error { reason } if reason.contains("out of range")),
            "got: {:?}",
            result.response
        );

        // until_ms > now + 28d.
        let too_far = now + 29 * 24 * 60 * 60 * 1000;
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::TimeoutMember {
                member_key: target_pk.clone(),
                until_ms: too_far,
                reason: None,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(
            matches!(&result.response, ServerResponse::Error { reason } if reason.contains("out of range"))
        );
    }

    #[test]
    fn test_timeout_member_happy_path() {
        let (conn, owner_pk) = setup();
        let target_pk = add_member(&conn, "Target");
        let until = current_unix_ms() + 60_000;

        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::TimeoutMember {
                member_key: target_pk.clone(),
                until_ms: until,
                reason: Some("warning".into()),
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        // Expect MemberTimeoutChanged plus an AuditEventCreated row.
        let timeout_evt = result
            .events
            .iter()
            .find(|e| matches!(e.event, ServerEvent::MemberTimeoutChanged { .. }))
            .expect("missing MemberTimeoutChanged event");
        match &timeout_evt.event {
            ServerEvent::MemberTimeoutChanged {
                until_ms: Some(u),
                reason: Some(r),
                ..
            } => {
                assert_eq!(*u, until);
                assert_eq!(r, "warning");
            }
            other => panic!("expected MemberTimeoutChanged, got {other:?}"),
        }
    }

    #[test]
    fn test_remove_timeout_clears() {
        let (conn, owner_pk) = setup();
        let target_pk = add_member(&conn, "Target");
        members::set_timeout(&conn, &target_pk, current_unix_ms() + 60_000, Some("oops")).unwrap();

        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::RemoveTimeout {
                member_key: target_pk.clone(),
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_eq!(
            members::is_timed_out(&conn, &target_pk, current_unix_ms()).unwrap(),
            None
        );
    }

    // -----------------------------------------------------------------------
    // Audit event emission tests (Task 7 / P2.7)
    // -----------------------------------------------------------------------

    use crate::audit;

    fn assert_has_audit_event(events: &[BroadcastEvent]) {
        let has = events
            .iter()
            .any(|e| matches!(e.event, ServerEvent::AuditEventCreated { .. }));
        assert!(has, "missing AuditEventCreated event in events vec");
    }

    #[test]
    fn test_kick_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let target_pk = add_member(&conn, "Target");
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::KickMember { member_key: target_pk.clone() },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "kick");
    }

    #[test]
    fn test_ban_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let target_pk = add_member(&conn, "Target");
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::BanMember {
                member_key: target_pk.clone(),
                reason: Some("spam".into()),
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "ban");
        assert_eq!(events[0].metadata["reason"], "spam");
    }

    #[test]
    fn test_kick_emits_you_were_kicked_to_target() {
        let (conn, owner_pk) = setup();
        let target_pk = add_member(&conn, "Target");
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::KickMember { member_key: target_pk.clone() },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        let has_targeted_kick = result.events.iter().any(|e| {
            matches!(e.event, ServerEvent::YouWereKicked)
                && matches!(&e.target, EventTarget::Members(pks) if pks.contains(&target_pk))
        });
        assert!(has_targeted_kick, "expected YouWereKicked targeted at the kicked member");
    }

    #[test]
    fn test_ban_emits_you_were_banned_to_target() {
        let (conn, owner_pk) = setup();
        let target_pk = add_member(&conn, "Target");
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::BanMember {
                member_key: target_pk.clone(),
                reason: Some("spam".into()),
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        let has_targeted_ban = result.events.iter().any(|e| {
            matches!(&e.event, ServerEvent::YouWereBanned { reason: Some(r) } if r == "spam")
                && matches!(&e.target, EventTarget::Members(pks) if pks.contains(&target_pk))
        });
        assert!(has_targeted_ban, "expected YouWereBanned targeted at banned member");
    }

    #[test]
    fn test_unban_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let target_pk = add_member(&conn, "Target");
        members::ban_member(&conn, &target_pk, Some("test")).unwrap();
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::UnbanMember { member_key: target_pk.clone() },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "unban");
    }

    #[test]
    fn test_timeout_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let target_pk = add_member(&conn, "Target");
        let until = current_unix_ms() + 60_000;
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::TimeoutMember {
                member_key: target_pk.clone(),
                until_ms: until,
                reason: Some("warning".into()),
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "timeout");
        assert_eq!(events[0].metadata["until_ms"], until);
        assert_eq!(events[0].metadata["reason"], "warning");
    }

    #[test]
    fn test_untimeout_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let target_pk = add_member(&conn, "Target");
        members::set_timeout(&conn, &target_pk, current_unix_ms() + 60_000, None).unwrap();
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::RemoveTimeout { member_key: target_pk.clone() },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "untimeout");
    }

    #[test]
    fn test_assign_role_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let target_pk = add_member(&conn, "Target");
        let role_id = members::create_role(
            &conn,
            "Helper",
            permissions::DEFAULT_EVERYONE,
            None,
            1,
            false,
            false,
        )
        .unwrap();
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::AssignRole {
                member_key: target_pk.clone(),
                role_id,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "role_assigned");
        assert_eq!(events[0].metadata["role_id"], role_id);
        assert_eq!(events[0].metadata["role_name"], "Helper");
    }

    #[test]
    fn test_remove_role_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let target_pk = add_member(&conn, "Target");
        let role_id = members::create_role(
            &conn,
            "Helper",
            permissions::DEFAULT_EVERYONE,
            None,
            1,
            false,
            false,
        )
        .unwrap();
        members::assign_role(&conn, &target_pk, role_id).unwrap();
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::RemoveRole {
                member_key: target_pk.clone(),
                role_id,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "role_removed");
        assert_eq!(events[0].metadata["role_id"], role_id);
        assert_eq!(events[0].metadata["role_name"], "Helper");
    }

    #[test]
    fn test_create_channel_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::CreateChannel {
                name: "audit-test".into(),
                channel_type: ChannelType::Text,
                category_id: None,
                position: Some(0),
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "channel_created");
        assert_eq!(events[0].metadata["channel_name"], "audit-test");
    }

    #[test]
    fn test_delete_channel_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let channel_id = channels::create_channel(&conn, "doomed", ChannelType::Text, None, 0).unwrap();
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::DeleteChannel { channel_id },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "channel_deleted");
        assert_eq!(events[0].metadata["channel_id"], channel_id);
        assert_eq!(events[0].metadata["channel_name"], "doomed");
    }

    #[test]
    fn test_update_channel_rename_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let channel_id = channels::create_channel(&conn, "old-name", ChannelType::Text, None, 0).unwrap();
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::UpdateChannel {
                channel_id,
                name: Some("new-name".into()),
                topic: None,
                nsfw: None,
                slow_mode_secs: None,
                retention_secs: None,
                category_id: None,
                position: None,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "channel_renamed");
        assert_eq!(events[0].metadata["old_name"], "old-name");
        assert_eq!(events[0].metadata["new_name"], "new-name");

        // Updating without changing name should NOT emit a new audit row.
        let _ = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::UpdateChannel {
                channel_id,
                name: Some("new-name".into()),
                topic: Some("topic only".into()),
                nsfw: None,
                slow_mode_secs: None,
                retention_secs: None,
                category_id: None,
                position: None,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        let events_after = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events_after.len(), 1, "no-op rename must not emit audit");
    }

    #[test]
    fn test_create_role_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::CreateRole {
                name: "AuditRole".into(),
                permissions: permissions::MANAGE_MESSAGES,
                color: None,
                position: Some(1),
                hoist: None,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "role_created");
        assert_eq!(events[0].metadata["role_name"], "AuditRole");
        assert_eq!(
            events[0].metadata["permissions"],
            permissions::MANAGE_MESSAGES.to_string()
        );
    }

    #[test]
    fn test_delete_role_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let role_id = members::create_role(&conn, "Doomed", 0, None, 1, false, false).unwrap();
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::DeleteRole { role_id },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "role_deleted");
        assert_eq!(events[0].metadata["role_id"], role_id);
        assert_eq!(events[0].metadata["role_name"], "Doomed");
    }

    #[test]
    fn test_update_role_perms_changed_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let role_id = members::create_role(&conn, "Changeling", 0, None, 1, false, false).unwrap();
        let new_perms = permissions::MANAGE_MESSAGES | permissions::KICK_MEMBERS;
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::UpdateRole {
                role_id,
                name: None,
                permissions: Some(new_perms),
                color: None,
                position: None,
                hoist: None,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "role_perms_changed");
        assert_eq!(events[0].metadata["old_permissions"], "0");
        assert_eq!(events[0].metadata["new_permissions"], new_perms.to_string());

        // Calling UpdateRole with same permissions value should NOT emit another row.
        let _ = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::UpdateRole {
                role_id,
                name: None,
                permissions: Some(new_perms),
                color: None,
                position: None,
                hoist: None,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        let events_after = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events_after.len(), 1, "no-op perms update must not emit audit");
    }

    #[test]
    fn test_set_channel_override_emits_audit_event() {
        let (conn, owner_pk) = setup();
        let channel_id = make_channel(&conn);
        let role_id = members::create_role(&conn, "OverrideRole", 0, None, 1, false, false).unwrap();
        let allow = permissions::SEND_MESSAGES;
        let deny = permissions::MANAGE_MESSAGES;
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::SetChannelOverride {
                channel_id,
                role_id,
                allow,
                deny,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_has_audit_event(&result.events);
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "channel_overrides_changed");
        assert_eq!(events[0].metadata["channel_id"], channel_id);
        assert_eq!(events[0].metadata["role_id"], role_id);
        assert_eq!(events[0].metadata["allow"], allow.to_string());
        assert_eq!(events[0].metadata["deny"], deny.to_string());
    }

    // -----------------------------------------------------------------------
    // ListAuditEvents handler tests (Task 8 / P2.8)
    // -----------------------------------------------------------------------

    #[test]
    fn test_list_audit_events_requires_manage_server() {
        let (conn, _owner_pk) = setup();
        let actor_pk = add_member(&conn, "Actor");
        let result = handle_request(
            &conn,
            &actor_pk,
            false,
            ServerRequest::ListAuditEvents {
                before_id: None,
                limit: 10,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        match result.response {
            ServerResponse::Error { reason } => {
                assert!(reason.contains("MANAGE_SERVER"), "got: {reason}")
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn test_list_audit_events_returns_paginated() {
        let (conn, owner_pk) = setup();
        for _ in 0..3 {
            audit::insert(&conn, &owner_pk, None, "test", serde_json::json!({})).unwrap();
        }
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::ListAuditEvents {
                before_id: None,
                limit: 2,
            },
            "/tmp",
            &fake_state(),
        )
        .unwrap();
        match result.response {
            ServerResponse::AuditEventsList { events } => {
                assert_eq!(events.len(), 2);
            }
            other => panic!("expected AuditEventsList, got {other:?}"),
        }
    }

    #[test]
    fn test_set_mute_broadcasts_stream_state_changed() {
        let (conn, owner) = setup();
        let state = fake_state();
        handle_request(&conn, &owner, true,
            ServerRequest::JoinStream { channel_id: 7 }, "", &state).unwrap();

        let result = handle_request(&conn, &owner, true,
            ServerRequest::SetMute { muted: true }, "", &state).unwrap();

        match result.response { ServerResponse::Ok => {} o => panic!("expected Ok, got {:?}", o) }
        assert_eq!(result.events.len(), 1, "SetMute should broadcast one event");
        match &result.events[0].event {
            ServerEvent::StreamStateChanged { session_id: _, muted, deafened, channel_id } => {
                assert_eq!(*channel_id, 7);
                assert!(*muted, "muted flag should be true");
                assert!(!*deafened, "deafened should remain false");
            }
            o => panic!("expected StreamStateChanged, got {:?}", o),
        }
    }

    #[test]
    fn test_set_deafen_broadcasts_stream_state_changed() {
        let (conn, owner) = setup();
        let state = fake_state();
        handle_request(&conn, &owner, true,
            ServerRequest::JoinStream { channel_id: 7 }, "", &state).unwrap();

        let result = handle_request(&conn, &owner, true,
            ServerRequest::SetDeafen { deafened: true }, "", &state).unwrap();

        assert_eq!(result.events.len(), 1, "SetDeafen should broadcast one event");
        match &result.events[0].event {
            ServerEvent::StreamStateChanged { deafened, .. } => assert!(*deafened),
            o => panic!("expected StreamStateChanged, got {:?}", o),
        }
    }

    #[test]
    fn test_set_mute_preserves_existing_deafen_flag() {
        let (conn, owner) = setup();
        let state = fake_state();
        handle_request(&conn, &owner, true,
            ServerRequest::JoinStream { channel_id: 7 }, "", &state).unwrap();
        handle_request(&conn, &owner, true,
            ServerRequest::SetDeafen { deafened: true }, "", &state).unwrap();

        // Muting while already deafened must broadcast BOTH flags set, not clobber deafen.
        let result = handle_request(&conn, &owner, true,
            ServerRequest::SetMute { muted: true }, "", &state).unwrap();

        assert_eq!(result.events.len(), 1, "SetMute should broadcast one event");
        match &result.events[0].event {
            ServerEvent::StreamStateChanged { muted, deafened, .. } => {
                assert!(*muted, "muted flag should be true");
                assert!(*deafened, "deafened flag must be preserved");
            }
            o => panic!("expected StreamStateChanged, got {:?}", o),
        }
    }

    fn make_profile(kp: &farder_crypto::identity::Keypair, status: Option<&str>) -> Vec<u8> {
        farder_crypto::profile::SignedProfile::create(
            kp, "Tester".to_string(), None, status.map(|s| s.to_string()),
        ).to_bytes()
    }

    #[test]
    fn test_update_profile_stores_and_broadcasts() {
        let (conn, _owner_pk) = setup();
        let kp = farder_crypto::identity::Keypair::generate();
        members::register_member(&conn, &kp.public_key(), "Tester").unwrap();

        let blob = make_profile(&kp, Some("hello"));
        let expected_hash = farder_crypto::profile::profile_hash_hex(&blob);

        let result = handle_request(
            &conn, &kp.public_key(), false,
            ServerRequest::UpdateProfile { profile: blob.clone() },
            "", &fake_state(),
        ).unwrap();

        assert!(matches!(result.response, ServerResponse::Ok));
        assert_eq!(result.events.len(), 1);
        match &result.events[0].event {
            ServerEvent::MemberProfileUpdated { public_key, profile_hash } => {
                assert_eq!(public_key, &kp.public_key());
                assert_eq!(profile_hash.as_deref(), Some(expected_hash.as_str()));
            }
            other => panic!("expected MemberProfileUpdated, got {:?}", other),
        }
        assert_eq!(members::get_member_profile(&conn, &kp.public_key()).unwrap().as_deref(), Some(&blob[..]));
    }

    #[test]
    fn test_update_profile_rejects_wrong_key() {
        let (conn, _owner_pk) = setup();
        let kp_signer = farder_crypto::identity::Keypair::generate();
        let mallory = add_member(&conn, "Mallory");
        let blob = make_profile(&kp_signer, None);

        let result = handle_request(
            &conn, &mallory, false,
            ServerRequest::UpdateProfile { profile: blob },
            "", &fake_state(),
        ).unwrap();
        assert!(matches!(result.response, ServerResponse::Error { .. }));
        assert!(members::get_member_profile(&conn, &mallory).unwrap().is_none());
    }

    #[test]
    fn test_update_profile_rejects_tampered_and_oversize_status() {
        let (conn, _owner_pk) = setup();
        let kp = farder_crypto::identity::Keypair::generate();
        members::register_member(&conn, &kp.public_key(), "Tester").unwrap();

        let mut blob = make_profile(&kp, Some("ok"));
        let mid = blob.len() / 2;
        blob[mid] ^= 0xFF;
        let result = handle_request(
            &conn, &kp.public_key(), false,
            ServerRequest::UpdateProfile { profile: blob },
            "", &fake_state(),
        ).unwrap();
        assert!(matches!(result.response, ServerResponse::Error { .. }));

        let long = "x".repeat(129);
        let blob = make_profile(&kp, Some(&long));
        let result = handle_request(
            &conn, &kp.public_key(), false,
            ServerRequest::UpdateProfile { profile: blob },
            "", &fake_state(),
        ).unwrap();
        assert!(matches!(result.response, ServerResponse::Error { .. }));

        // Third leg: display_name of 129 chars should be rejected.
        let long_name = "y".repeat(129);
        let blob = farder_crypto::profile::SignedProfile::create(
            &kp, long_name, None, None,
        ).to_bytes();
        let result = handle_request(
            &conn, &kp.public_key(), false,
            ServerRequest::UpdateProfile { profile: blob },
            "", &fake_state(),
        ).unwrap();
        assert!(matches!(result.response, ServerResponse::Error { .. }));
    }

    #[test]
    fn test_get_member_profile_roundtrip_and_members_hash() {
        let (conn, owner_pk) = setup();
        let kp = farder_crypto::identity::Keypair::generate();
        members::register_member(&conn, &kp.public_key(), "Tester").unwrap();
        let blob = make_profile(&kp, None);
        let hash = farder_crypto::profile::profile_hash_hex(&blob);
        handle_request(
            &conn, &kp.public_key(), false,
            ServerRequest::UpdateProfile { profile: blob.clone() },
            "", &fake_state(),
        ).unwrap();

        let result = handle_request(
            &conn, &owner_pk, true,
            ServerRequest::GetMemberProfile { member_key: kp.public_key() },
            "", &fake_state(),
        ).unwrap();
        match result.response {
            ServerResponse::MemberProfile { member_key, profile } => {
                assert_eq!(member_key, kp.public_key());
                assert_eq!(profile.as_deref(), Some(&blob[..]));
            }
            other => panic!("expected MemberProfile, got {:?}", other),
        }

        let result = handle_request(
            &conn, &owner_pk, true, ServerRequest::GetMembers, "", &fake_state(),
        ).unwrap();
        match result.response {
            ServerResponse::Members { members: infos } => {
                let me = infos.iter().find(|m| m.public_key == kp.public_key()).unwrap();
                assert_eq!(me.profile_hash.as_deref(), Some(hash.as_str()));
            }
            other => panic!("expected Members, got {:?}", other),
        }
    }

    #[test]
    fn test_update_profile_rejects_oversize_blob() {
        let (conn, _owner_pk) = setup();
        let kp = farder_crypto::identity::Keypair::generate();
        members::register_member(&conn, &kp.public_key(), "Tester").unwrap();
        // A signed profile whose avatar pushes the whole blob past 2.5 MB.
        let blob = farder_crypto::profile::SignedProfile::create(
            &kp, "Tester".to_string(), Some(vec![0u8; 2_700_000]), None,
        ).to_bytes();
        assert!(blob.len() > 2_621_440);
        let result = handle_request(
            &conn, &kp.public_key(), false,
            ServerRequest::UpdateProfile { profile: blob },
            "", &fake_state(),
        ).unwrap();
        assert!(matches!(result.response, ServerResponse::Error { .. }));
        assert!(members::get_member_profile(&conn, &kp.public_key()).unwrap().is_none());
    }

    #[test]
    fn test_update_profile_rejects_invalid_avatar_bytes() {
        let (conn, _owner_pk) = setup();
        let kp = farder_crypto::identity::Keypair::generate();
        members::register_member(&conn, &kp.public_key(), "Tester").unwrap();
        // Garbage avatar bytes (not a valid image) inside a correctly signed profile.
        let blob = farder_crypto::profile::SignedProfile::create(
            &kp, "Tester".to_string(), Some(vec![0xABu8; 1024]), None,
        ).to_bytes();
        let result = handle_request(
            &conn, &kp.public_key(), false,
            ServerRequest::UpdateProfile { profile: blob },
            "", &fake_state(),
        ).unwrap();
        assert!(matches!(result.response, ServerResponse::Error { .. }));
        assert!(members::get_member_profile(&conn, &kp.public_key()).unwrap().is_none());
    }

    #[test]
    fn test_update_profile_accepts_valid_png_avatar() {
        let (conn, _owner_pk) = setup();
        let kp = farder_crypto::identity::Keypair::generate();
        members::register_member(&conn, &kp.public_key(), "Tester").unwrap();
        // Encode a real 1x1 PNG with the image crate (already a dependency).
        let mut png_bytes: Vec<u8> = Vec::new();
        let img = image::RgbaImage::new(1, 1);
        img.write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png).unwrap();
        let blob = farder_crypto::profile::SignedProfile::create(
            &kp, "Tester".to_string(), Some(png_bytes), Some("with avatar".to_string()),
        ).to_bytes();
        let result = handle_request(
            &conn, &kp.public_key(), false,
            ServerRequest::UpdateProfile { profile: blob.clone() },
            "", &fake_state(),
        ).unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_eq!(members::get_member_profile(&conn, &kp.public_key()).unwrap().as_deref(), Some(&blob[..]));
    }

    // -----------------------------------------------------------------------
    // UpdatePresence tests
    // -----------------------------------------------------------------------

    fn make_presence() -> farder_protocol::server::Presence {
        farder_protocol::server::Presence {
            kind: farder_protocol::server::PresenceKind::Music,
            details: "Song Title".to_string(),
            state: Some("Artist Name".to_string()),
        }
    }

    #[test]
    fn test_update_presence_some_stores_and_broadcasts() {
        let (conn, owner_pk) = setup();
        let state = fake_state();
        let presence = make_presence();

        let result = handle_request(
            &conn, &owner_pk, true,
            ServerRequest::UpdatePresence { presence: Some(presence.clone()) },
            "", &state,
        ).unwrap();

        assert!(matches!(result.response, ServerResponse::Ok), "expected Ok");
        assert_eq!(result.events.len(), 1, "should have one broadcast event");

        match &result.events[0] {
            BroadcastEvent { target: EventTarget::All, event: ServerEvent::MemberPresenceUpdated { public_key, presence: p } } => {
                assert_eq!(public_key, &owner_pk, "broadcast pk should be authenticated sender");
                assert_eq!(p.as_ref(), Some(&presence));
            }
            other => panic!("unexpected event: {:?}", other),
        }

        // Confirm presence is stored in the map under the sender's key.
        let stored = state.presences.read().unwrap().get(owner_pk.as_bytes()).cloned();
        assert_eq!(stored, Some(presence));
    }

    #[test]
    fn test_update_presence_none_removes_and_broadcasts() {
        let (conn, owner_pk) = setup();
        let state = fake_state();
        let presence = make_presence();

        // First set a presence.
        handle_request(
            &conn, &owner_pk, true,
            ServerRequest::UpdatePresence { presence: Some(presence) },
            "", &state,
        ).unwrap();
        assert!(state.presences.read().unwrap().contains_key(owner_pk.as_bytes()));

        // Now clear it.
        let result = handle_request(
            &conn, &owner_pk, true,
            ServerRequest::UpdatePresence { presence: None },
            "", &state,
        ).unwrap();

        assert!(matches!(result.response, ServerResponse::Ok));
        assert_eq!(result.events.len(), 1);
        match &result.events[0] {
            BroadcastEvent { target: EventTarget::All, event: ServerEvent::MemberPresenceUpdated { public_key, presence: p } } => {
                assert_eq!(public_key, &owner_pk);
                assert!(p.is_none(), "broadcast presence should be None");
            }
            other => panic!("unexpected event: {:?}", other),
        }
        // Confirm removed from map.
        assert!(state.presences.read().unwrap().get(owner_pk.as_bytes()).is_none());
    }

    #[test]
    fn test_update_presence_details_too_long_returns_error() {
        let (conn, owner_pk) = setup();
        let state = fake_state();
        let presence = farder_protocol::server::Presence {
            kind: farder_protocol::server::PresenceKind::Game,
            details: "x".repeat(129),
            state: None,
        };

        let result = handle_request(
            &conn, &owner_pk, true,
            ServerRequest::UpdatePresence { presence: Some(presence) },
            "", &state,
        ).unwrap();

        assert!(matches!(result.response, ServerResponse::Error { .. }), "expected Error for overlong details");
        // Nothing should be stored.
        assert!(state.presences.read().unwrap().get(owner_pk.as_bytes()).is_none());
    }

    #[test]
    fn test_update_presence_state_too_long_returns_error() {
        let (conn, owner_pk) = setup();
        let state = fake_state();
        let presence = farder_protocol::server::Presence {
            kind: farder_protocol::server::PresenceKind::Music,
            details: "Track".to_string(),
            state: Some("a".repeat(129)),
        };

        let result = handle_request(
            &conn, &owner_pk, true,
            ServerRequest::UpdatePresence { presence: Some(presence) },
            "", &state,
        ).unwrap();

        assert!(matches!(result.response, ServerResponse::Error { .. }), "expected Error for overlong state");
        assert!(state.presences.read().unwrap().get(owner_pk.as_bytes()).is_none());
    }

    #[test]
    fn test_get_members_includes_stored_presence() {
        let (conn, owner_pk) = setup();
        let state = fake_state();
        let presence = make_presence();

        // Store presence for owner.
        handle_request(
            &conn, &owner_pk, true,
            ServerRequest::UpdatePresence { presence: Some(presence.clone()) },
            "", &state,
        ).unwrap();

        let result = handle_request(
            &conn, &owner_pk, true,
            ServerRequest::GetMembers,
            "", &state,
        ).unwrap();

        match result.response {
            ServerResponse::Members { members: infos } => {
                let me = infos.iter().find(|m| m.public_key == owner_pk).unwrap();
                assert_eq!(me.presence.as_ref(), Some(&presence), "GetMembers should include stored presence");
            }
            other => panic!("expected Members, got {:?}", other),
        }
    }

    #[test]
    fn test_update_presence_keyed_to_authenticated_sender() {
        let (conn, owner_pk) = setup();
        let alice = add_member(&conn, "Alice");
        let bob = add_member(&conn, "Bob");
        let alice_state = fake_state();
        let presence = make_presence();

        // Alice sets her presence; it should be stored under Alice's key, NOT Bob's.
        handle_request(
            &conn, &alice, false,
            ServerRequest::UpdatePresence { presence: Some(presence.clone()) },
            "", &alice_state,
        ).unwrap();

        assert_eq!(alice_state.presences.read().unwrap().get(alice.as_bytes()).cloned(), Some(presence.clone()));
        assert!(alice_state.presences.read().unwrap().get(bob.as_bytes()).is_none(), "Bob should have no presence");
        assert!(alice_state.presences.read().unwrap().get(owner_pk.as_bytes()).is_none(), "owner should have no presence");
    }

    // -----------------------------------------------------------------------
    // SubmitEvent integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_submit_event_accept_and_broadcast() {
        use crate::event_ingest;
        use farder_crypto::event_log::{DeviceCert, Event, EventPayload as EP, Genesis};
        use farder_crypto::event_log_state::LogState;

        // Build the state: open in-memory db with roles + owner member.
        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        // Set up @everyone role so members can be created.
        let everyone_id = members::create_role(
            &conn,
            "@everyone",
            permissions::DEFAULT_EVERYONE,
            None,
            0,
            true,
            false,
        )
        .unwrap();

        // Owner keypair and device keypair.
        let owner_kp = Keypair::generate();
        let dev_kp = Keypair::generate();

        // Register the owner as a server member so get_message works.
        members::register_member(&conn, &owner_kp.public_key(), "Owner").unwrap();
        members::assign_role(&conn, &owner_kp.public_key(), everyone_id).unwrap();

        // Create genesis and initialize log state.
        let g = Genesis {
            version: 1,
            name: "t".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        event_ingest::save_genesis(&conn, &g).unwrap();
        *state.log_state.lock().unwrap() = Some(LogState::from_genesis(&g));

        // Create a channel so MessagePosted channel_id validation succeeds.
        let channel_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();

        // Build event chain: owner DeviceAuthorized, then MessagePosted.
        let cert = DeviceCert::create(&owner_kp, &dev_kp.public_key(), 1);
        let da = Event::next(
            &dev_kp,
            owner_kp.public_key(),
            g.server_id(),
            None,
            0,
            1,
            EP::DeviceAuthorized { cert },
        );
        let msg = Event::next(
            &dev_kp,
            owner_kp.public_key(),
            g.server_id(),
            Some(&da),
            1,
            2,
            EP::MessagePosted {
                channel_id,
                content: "hello from event log".into(),
                reply_to: None,
                attachments: vec![],
            },
        );

        // Submit DeviceAuthorized — should be accepted.
        let da_result = handle_request(
            &conn,
            &owner_kp.public_key(),
            true,
            ServerRequest::SubmitEvent { event: da.clone() },
            "",
            &state,
        )
        .unwrap();
        match da_result.response {
            ServerResponse::EventAccepted { event_hash, .. } => {
                assert_eq!(event_hash, da.hash(), "DA event_hash should match");
            }
            other => panic!("expected EventAccepted for DeviceAuthorized, got {:?}", other),
        }
        // DA produces no NewMessage broadcast.
        assert!(da_result.events.is_empty(), "DeviceAuthorized should not produce broadcast events");

        // Submit MessagePosted — should be accepted with a NewMessage broadcast.
        let msg_result = handle_request(
            &conn,
            &owner_kp.public_key(),
            true,
            ServerRequest::SubmitEvent { event: msg.clone() },
            "",
            &state,
        )
        .unwrap();
        match &msg_result.response {
            ServerResponse::EventAccepted { event_hash, .. } => {
                assert_eq!(*event_hash, msg.hash(), "MessagePosted event_hash should match");
            }
            other => panic!("expected EventAccepted for MessagePosted, got {:?}", other),
        }
        // Should have exactly one NewMessage broadcast.
        assert_eq!(msg_result.events.len(), 1, "should have one broadcast event for MessagePosted");
        match &msg_result.events[0].event {
            ServerEvent::NewMessage { .. } => {}
            other => panic!("expected NewMessage broadcast, got {:?}", other),
        }

        // The messages table should have one row with the correct event_hash.
        let (content, eh): (String, String) = conn
            .query_row(
                "SELECT content, event_hash FROM messages WHERE event_hash = ?1",
                rusqlite::params![msg.hash()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("message row should exist in messages table");
        assert_eq!(content, "hello from event log");
        assert_eq!(eh, msg.hash());
    }

    #[test]
    fn test_submit_event_no_genesis_returns_error() {
        use farder_crypto::event_log::{DeviceCert, Event, EventPayload as EP, Genesis};

        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        // Do NOT initialize log_state — it stays None.
        let owner_kp = Keypair::generate();
        let dev_kp = Keypair::generate();
        let g = Genesis {
            version: 1,
            name: "t".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        let cert = DeviceCert::create(&owner_kp, &dev_kp.public_key(), 1);
        let da = Event::next(
            &dev_kp,
            owner_kp.public_key(),
            g.server_id(),
            None,
            0,
            1,
            EP::DeviceAuthorized { cert },
        );

        let result = handle_request(
            &conn,
            &owner_kp.public_key(),
            true,
            ServerRequest::SubmitEvent { event: da },
            "",
            &state,
        )
        .unwrap();
        match result.response {
            ServerResponse::Error { reason } => {
                assert!(reason.contains("no genesis"), "expected 'no genesis' in error, got: {}", reason);
            }
            other => panic!("expected Error response, got {:?}", other),
        }
    }

    #[test]
    fn test_submit_event_non_member_rejected() {
        // Note: DeviceAuthorized for any identity is accepted by the log (it's the
        // device-bootstrap mechanism; anyone may register their own device). The
        // membership restriction fires for MessagePosted. This test verifies that a
        // stranger who registered their device but was never added as a member cannot
        // post a message.
        use crate::event_ingest;
        use farder_crypto::event_log::{DeviceCert, Event, EventPayload as EP, Genesis};
        use farder_crypto::event_log_state::LogState;

        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        members::create_role(&conn, "@everyone", permissions::DEFAULT_EVERYONE, None, 0, true, false).unwrap();

        let owner_kp = Keypair::generate();
        let g = Genesis {
            version: 1,
            name: "t".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        event_ingest::save_genesis(&conn, &g).unwrap();
        *state.log_state.lock().unwrap() = Some(LogState::from_genesis(&g));

        let channel_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();

        // Stranger registers their device (DeviceAuthorized succeeds — anyone can do
        // this; it adds the device to the log but does NOT make them a member).
        let stranger_kp = Keypair::generate();
        let stranger_dev_kp = Keypair::generate();
        let stranger_cert = DeviceCert::create(&stranger_kp, &stranger_dev_kp.public_key(), 1);
        let stranger_da = Event::next(
            &stranger_dev_kp,
            stranger_kp.public_key(),
            g.server_id(),
            None,
            0,
            1,
            EP::DeviceAuthorized { cert: stranger_cert },
        );

        let da_result = handle_request(
            &conn,
            &stranger_kp.public_key(),
            false,
            ServerRequest::SubmitEvent { event: stranger_da.clone() },
            "",
            &state,
        )
        .unwrap();
        // DeviceAuthorized accepted — the stranger's device is registered.
        match da_result.response {
            ServerResponse::EventAccepted { .. } => {}
            other => panic!("expected EventAccepted for stranger's DeviceAuthorized, got {:?}", other),
        }

        // Now try a MessagePosted from the stranger's device — rejected because
        // the stranger is not a member (only their device is registered).
        let stranger_msg = Event::next(
            &stranger_dev_kp,
            stranger_kp.public_key(),
            g.server_id(),
            Some(&stranger_da),
            1,
            2,
            EP::MessagePosted {
                channel_id,
                content: "hacked".into(),
                reply_to: None,
                attachments: vec![],
            },
        );

        let msg_result = handle_request(
            &conn,
            &stranger_kp.public_key(),
            false,
            ServerRequest::SubmitEvent { event: stranger_msg },
            "",
            &state,
        )
        .unwrap();
        match msg_result.response {
            ServerResponse::Error { reason } => {
                assert!(
                    reason.contains("event rejected"),
                    "expected 'event rejected' in error, got: {}",
                    reason
                );
            }
            other => panic!("expected Error for non-member MessagePosted, got {:?}", other),
        }
    }

    #[test]
    fn non_member_is_gated_from_content_but_can_bootstrap() {
        use crate::event_ingest;
        use farder_crypto::event_log::Genesis;
        use farder_crypto::event_log_state::LogState;

        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        members::create_role(&conn, "@everyone", permissions::DEFAULT_EVERYONE, None, 0, true, false).unwrap();

        let owner_kp = Keypair::generate();
        let everyone_id: u64 = conn
            .query_row("SELECT id FROM roles WHERE name = '@everyone'", [], |row| {
                Ok(row.get::<_, i64>(0)? as u64)
            })
            .unwrap();
        members::register_member(&conn, &owner_kp.public_key(), "Owner").unwrap();
        members::assign_role(&conn, &owner_kp.public_key(), everyone_id).unwrap();

        let g = Genesis {
            version: 1,
            name: "mesh-test".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        event_ingest::save_genesis(&conn, &g).unwrap();
        *state.log_state.lock().unwrap() = Some(LogState::from_genesis(&g));

        // A stranger who is NOT a log member.
        let stranger = Keypair::generate().public_key();

        // Gated request (FetchHistory) is rejected for the non-member.
        let r = handle_request(
            &conn,
            &stranger,
            false,
            ServerRequest::FetchHistory { channel_id: 1, before_id: None, limit: 50 },
            "",
            &state,
        )
        .unwrap();
        assert!(
            matches!(r.response, ServerResponse::Error { .. }),
            "non-member must be denied content"
        );

        // The owner (a log member) is allowed.
        let r2 = handle_request(
            &conn,
            &owner_kp.public_key(),
            true,
            ServerRequest::FetchHistory { channel_id: 1, before_id: None, limit: 50 },
            "",
            &state,
        )
        .unwrap();
        assert!(
            !matches!(&r2.response, ServerResponse::Error { reason } if reason.contains("member")),
            "a log member must not be gated"
        );

        // Allow-listed bootstrap request (GetServerInfo) is permitted for the non-member.
        let r3 = handle_request(
            &conn,
            &stranger,
            false,
            ServerRequest::GetServerInfo,
            "",
            &state,
        )
        .unwrap();
        assert!(
            !matches!(&r3.response, ServerResponse::Error { reason } if reason.contains("member")),
            "bootstrap requests must stay open to non-members"
        );
    }

    #[test]
    fn test_submit_event_bad_channel_rejected() {
        use crate::event_ingest;
        use farder_crypto::event_log::{DeviceCert, Event, EventPayload as EP, Genesis};
        use farder_crypto::event_log_state::LogState;

        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        members::create_role(&conn, "@everyone", permissions::DEFAULT_EVERYONE, None, 0, true, false).unwrap();

        let owner_kp = Keypair::generate();
        let dev_kp = Keypair::generate();
        let g = Genesis {
            version: 1,
            name: "t".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        event_ingest::save_genesis(&conn, &g).unwrap();
        *state.log_state.lock().unwrap() = Some(LogState::from_genesis(&g));

        // Create channel (id will be 1), then we'll post to id=999 which doesn't exist.
        channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();

        // Authorize the owner's device first.
        let cert = DeviceCert::create(&owner_kp, &dev_kp.public_key(), 1);
        let da = Event::next(
            &dev_kp,
            owner_kp.public_key(),
            g.server_id(),
            None,
            0,
            1,
            EP::DeviceAuthorized { cert },
        );
        handle_request(
            &conn,
            &owner_kp.public_key(),
            true,
            ServerRequest::SubmitEvent { event: da.clone() },
            "",
            &state,
        )
        .unwrap();

        // Now post to a non-existent channel 999.
        let bad_msg = Event::next(
            &dev_kp,
            owner_kp.public_key(),
            g.server_id(),
            Some(&da),
            1,
            2,
            EP::MessagePosted {
                channel_id: 999,
                content: "bad channel".into(),
                reply_to: None,
                attachments: vec![],
            },
        );

        let result = handle_request(
            &conn,
            &owner_kp.public_key(),
            true,
            ServerRequest::SubmitEvent { event: bad_msg },
            "",
            &state,
        )
        .unwrap();
        match result.response {
            ServerResponse::Error { reason } => {
                assert_eq!(reason, "channel not found", "expected 'channel not found', got: {}", reason);
            }
            other => panic!("expected Error for bad channel, got {:?}", other),
        }
    }

    #[test]
    fn submit_event_with_valid_attachment_materializes_row() {
        use crate::event_ingest;
        use farder_crypto::event_log::{AttachmentCap, DeviceCert, Event, EventPayload as EP, Genesis};
        use farder_crypto::event_log_state::LogState;

        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        let everyone_id = members::create_role(
            &conn,
            "@everyone",
            permissions::DEFAULT_EVERYONE,
            None,
            0,
            true,
            false,
        )
        .unwrap();

        let owner_kp = Keypair::generate();
        let dev_kp = Keypair::generate();

        members::register_member(&conn, &owner_kp.public_key(), "Owner").unwrap();
        members::assign_role(&conn, &owner_kp.public_key(), everyone_id).unwrap();

        let g = Genesis {
            version: 1,
            name: "t".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        event_ingest::save_genesis(&conn, &g).unwrap();
        *state.log_state.lock().unwrap() = Some(LogState::from_genesis(&g));
        *state.genesis.lock().unwrap() = Some(g.clone());

        let channel_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();

        let cert = DeviceCert::create(&owner_kp, &dev_kp.public_key(), 1);
        let da = Event::next(
            &dev_kp,
            owner_kp.public_key(),
            g.server_id(),
            None,
            0,
            1,
            EP::DeviceAuthorized { cert },
        );

        // Submit DeviceAuthorized to advance log state.
        handle_request(
            &conn,
            &owner_kp.public_key(),
            true,
            ServerRequest::SubmitEvent { event: da.clone() },
            "",
            &state,
        )
        .unwrap();

        // Insert a blob owned by the owner.
        conn.execute(
            "INSERT INTO files (hash, size, mime_type, original_name, uploaded_by, uploaded_at, ref_count) \
             VALUES ('cafef00d', 5, 'image/png', 'pic.png', ?1, 1, 0)",
            rusqlite::params![owner_kp.public_key().as_bytes().as_slice()],
        )
        .unwrap();

        // Build a MessagePosted carrying a matching cap, chained after the device-auth event.
        let cap = AttachmentCap {
            content_hash: "cafef00d".into(),
            declared_type: "image/png".into(),
            size: 5,
            uploader: owner_kp.public_key(),
        };
        let msg = Event::next(
            &dev_kp,
            owner_kp.public_key(),
            g.server_id(),
            Some(&da),
            1,
            2,
            EP::MessagePosted {
                channel_id,
                content: "look".into(),
                reply_to: None,
                attachments: vec![cap],
            },
        );

        let result = handle_request(
            &conn,
            &owner_kp.public_key(),
            true,
            ServerRequest::SubmitEvent { event: msg.clone() },
            "",
            &state,
        )
        .unwrap();
        assert!(
            matches!(result.response, ServerResponse::EventAccepted { .. }),
            "expected acceptance, got {:?}",
            result.response
        );

        // The message_attachments row exists and points at the blob.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message_attachments ma JOIN files f ON f.id = ma.file_id WHERE f.hash = 'cafef00d'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn submit_event_attachment_redacted_takes_down_blob() {
        use crate::event_ingest;
        use farder_crypto::event_log::{AttachmentCap, DeviceCert, Event, EventPayload as EP, Genesis};
        use farder_crypto::event_log_state::LogState;

        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        let everyone_id = members::create_role(
            &conn,
            "@everyone",
            permissions::DEFAULT_EVERYONE,
            None,
            0,
            true,
            false,
        )
        .unwrap();

        let owner_kp = Keypair::generate();
        let dev_kp = Keypair::generate();

        members::register_member(&conn, &owner_kp.public_key(), "Owner").unwrap();
        members::assign_role(&conn, &owner_kp.public_key(), everyone_id).unwrap();

        let g = Genesis {
            version: 1,
            name: "t".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        event_ingest::save_genesis(&conn, &g).unwrap();
        *state.log_state.lock().unwrap() = Some(LogState::from_genesis(&g));
        *state.genesis.lock().unwrap() = Some(g.clone());

        let channel_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();

        let cert = DeviceCert::create(&owner_kp, &dev_kp.public_key(), 1);
        let da = Event::next(
            &dev_kp,
            owner_kp.public_key(),
            g.server_id(),
            None,
            0,
            1,
            EP::DeviceAuthorized { cert },
        );

        // Submit DeviceAuthorized to advance log state.
        handle_request(
            &conn,
            &owner_kp.public_key(),
            true,
            ServerRequest::SubmitEvent { event: da.clone() },
            "",
            &state,
        )
        .unwrap();

        // Insert a blob owned by the owner.
        conn.execute(
            "INSERT INTO files (hash, size, mime_type, original_name, uploaded_by, uploaded_at, ref_count) \
             VALUES ('cafef00d', 5, 'image/png', 'pic.png', ?1, 1, 0)",
            rusqlite::params![owner_kp.public_key().as_bytes().as_slice()],
        )
        .unwrap();

        // Build a MessagePosted carrying a matching cap, chained after the device-auth event.
        let cap = AttachmentCap {
            content_hash: "cafef00d".into(),
            declared_type: "image/png".into(),
            size: 5,
            uploader: owner_kp.public_key(),
        };
        let msg = Event::next(
            &dev_kp,
            owner_kp.public_key(),
            g.server_id(),
            Some(&da),
            1,
            2,
            EP::MessagePosted {
                channel_id,
                content: "look".into(),
                reply_to: None,
                attachments: vec![cap],
            },
        );

        handle_request(
            &conn,
            &owner_kp.public_key(),
            true,
            ServerRequest::SubmitEvent { event: msg.clone() },
            "",
            &state,
        )
        .unwrap();

        // Now submit AttachmentRedacted chained from the MessagePosted.
        let redact = Event::next(
            &dev_kp,
            owner_kp.public_key(),
            g.server_id(),
            Some(&msg),
            2,
            3,
            EP::AttachmentRedacted { content_hash: "cafef00d".into() },
        );

        let result = handle_request(
            &conn,
            &owner_kp.public_key(),
            true,
            ServerRequest::SubmitEvent { event: redact },
            "",
            &state,
        )
        .unwrap();
        assert!(
            matches!(result.response, ServerResponse::EventAccepted { .. }),
            "expected acceptance, got {:?}",
            result.response
        );

        // files.redacted_by set, message_attachments row still present.
        let redacted_by: Option<Vec<u8>> = conn
            .query_row(
                "SELECT redacted_by FROM files WHERE hash = 'cafef00d'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(redacted_by.is_some(), "redacted_by should be set");

        let att_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message_attachments ma JOIN files f ON f.id = ma.file_id WHERE f.hash = 'cafef00d'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(att_count, 1, "message_attachments row should still exist");
    }

    #[test]
    fn get_members_hides_non_log_members_on_mesh() {
        use crate::event_ingest;
        use farder_crypto::event_log::Genesis;
        use farder_crypto::event_log_state::LogState;

        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        members::create_role(&conn, "@everyone", permissions::DEFAULT_EVERYONE, None, 0, true, false).unwrap();

        let owner_kp = Keypair::generate();
        let everyone_id: u64 = conn
            .query_row("SELECT id FROM roles WHERE name = '@everyone'", [], |row| {
                Ok(row.get::<_, i64>(0)? as u64)
            })
            .unwrap();
        members::register_member(&conn, &owner_kp.public_key(), "Owner").unwrap();
        members::assign_role(&conn, &owner_kp.public_key(), everyone_id).unwrap();

        let g = Genesis {
            version: 1,
            name: "mesh-test".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        event_ingest::save_genesis(&conn, &g).unwrap();
        *state.log_state.lock().unwrap() = Some(LogState::from_genesis(&g));

        // A second identity registered in the legacy members table but NOT in the log.
        let pending = farder_crypto::identity::Keypair::generate().public_key();
        members::register_member(&conn, &pending, "vk_pending").unwrap();

        // GetMembers (called by the owner, a member) must include the owner and
        // EXCLUDE the non-log member.
        let owner = owner_kp.public_key();
        let r = handle_request(&conn, &owner, true, ServerRequest::GetMembers, "", &state).unwrap();
        let listed = match r.response {
            ServerResponse::Members { members } => members,
            other => panic!("expected Members, got {:?}", other),
        };
        let keys: Vec<_> = listed.iter().map(|m| m.public_key.clone()).collect();
        assert!(keys.contains(&owner), "owner (log member) shown");
        assert!(!keys.contains(&pending), "non-log member hidden from the member list");
    }

    #[test]
    fn membership_status_reports_member_pending_none() {
        use crate::event_ingest;
        use farder_crypto::event_log::Genesis;
        use farder_crypto::event_log_state::LogState;

        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        members::create_role(&conn, "@everyone", permissions::DEFAULT_EVERYONE, None, 0, true, false).unwrap();

        let owner_kp = Keypair::generate();
        let everyone_id: u64 = conn
            .query_row("SELECT id FROM roles WHERE name = '@everyone'", [], |row| {
                Ok(row.get::<_, i64>(0)? as u64)
            })
            .unwrap();
        members::register_member(&conn, &owner_kp.public_key(), "Owner").unwrap();
        members::assign_role(&conn, &owner_kp.public_key(), everyone_id).unwrap();

        let g = Genesis {
            version: 1,
            name: "mesh-test".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        event_ingest::save_genesis(&conn, &g).unwrap();
        *state.log_state.lock().unwrap() = Some(LogState::from_genesis(&g));

        let owner = owner_kp.public_key();
        let stranger = farder_crypto::identity::Keypair::generate().public_key();

        // Owner (log member) → "member"
        let r = handle_request(&conn, &owner, true, ServerRequest::GetMembershipStatus, "", &state).unwrap();
        assert!(
            matches!(r.response, ServerResponse::MembershipStatus { ref status } if status == "member"),
            "log member should get status 'member', got: {:?}",
            r.response
        );

        // Stranger (not a log member) → "none"
        let r2 = handle_request(&conn, &stranger, false, ServerRequest::GetMembershipStatus, "", &state).unwrap();
        assert!(
            matches!(r2.response, ServerResponse::MembershipStatus { ref status } if status == "none"),
            "non-member should get status 'none', got: {:?}",
            r2.response
        );
    }

    #[test]
    fn get_pending_members_lists_pending_and_requires_kick() {
        use crate::event_ingest;
        use farder_crypto::event_log::{DeviceCert, Event, EventPayload as EP, Genesis};
        use farder_crypto::event_log_state::LogState;

        // ---- Build mesh state: owner + @everyone role ----
        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        let everyone_id = members::create_role(
            &conn,
            "@everyone",
            permissions::DEFAULT_EVERYONE,
            None,
            0,
            true,
            false,
        )
        .unwrap();

        // Owner keypair + device.
        let owner_kp = Keypair::generate();
        let owner_dev = Keypair::generate();
        members::register_member(&conn, &owner_kp.public_key(), "Owner").unwrap();
        members::assign_role(&conn, &owner_kp.public_key(), everyone_id).unwrap();

        let g = Genesis {
            version: 1,
            name: "mesh-pending-test".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        event_ingest::save_genesis(&conn, &g).unwrap();
        let mut ls = LogState::from_genesis(&g);

        // ---- Drive an approval join through the LogState ----
        // Owner DeviceAuthorized.
        let owner_cert = DeviceCert::create(&owner_kp, &owner_dev.public_key(), 1);
        let owner_da = Event::next(
            &owner_dev,
            owner_kp.public_key(),
            g.server_id(),
            None,
            0,
            1,
            EP::DeviceAuthorized { cert: owner_cert },
        );
        ls.apply(&owner_da).unwrap();

        // Owner creates an approval-required invite.
        let inv = Event::next(
            &owner_dev,
            owner_kp.public_key(),
            g.server_id(),
            Some(&owner_da),
            1,
            2,
            EP::InviteCreated {
                code_hash: "testhash".into(),
                max_uses: 10,
                expires_at: 9999999999,
                requires_approval: true,
            },
        );
        ls.apply(&inv).unwrap();

        // Alice joins via that invite → pending.
        let alice_kp = Keypair::generate();
        let alice_dev = Keypair::generate();
        let alice_cert = DeviceCert::create(&alice_kp, &alice_dev.public_key(), 1);
        let alice_da = Event::next(
            &alice_dev,
            alice_kp.public_key(),
            g.server_id(),
            None,
            0,
            3,
            EP::DeviceAuthorized { cert: alice_cert },
        );
        ls.apply(&alice_da).unwrap();
        let alice_join = Event::next(
            &alice_dev,
            alice_kp.public_key(),
            g.server_id(),
            Some(&alice_da),
            1,
            4,
            EP::MemberJoined { member: alice_kp.public_key(), invite: inv.hash() },
        );
        ls.apply(&alice_join).unwrap();
        assert!(ls.is_pending(&alice_kp.public_key()), "alice must be pending after approval join");

        // Register alice in the legacy members table so get_member can resolve her.
        members::register_member(&conn, &alice_kp.public_key(), "Alice").unwrap();

        // Seed the live log_state.
        *state.log_state.lock().unwrap() = Some(ls);

        let owner_pk = owner_kp.public_key();
        let alice_pk = alice_kp.public_key();

        // ---- Owner (holds kick via is_owner) sees the pending list ----
        let r = handle_request(
            &conn,
            &owner_pk,
            true, // is_owner
            ServerRequest::GetPendingMembers,
            "",
            &state,
        )
        .unwrap();
        let pending = match r.response {
            ServerResponse::PendingMembers { members } => members,
            other => panic!("expected PendingMembers, got {:?}", other),
        };
        assert_eq!(pending.len(), 1, "exactly one pending member");
        assert_eq!(pending[0].public_key, alice_pk, "pending member is Alice");
        assert_eq!(pending[0].display_name, "Alice");
        assert!(pending[0].role_ids.is_empty(), "role_ids must be empty for pending members");
        assert!(pending[0].timeout_until.is_none());
        assert!(pending[0].presence.is_none());

        // ---- A member WITHOUT kick permission is denied ----
        // Build a plain member: registered in legacy table, IS a log member,
        // but @everyone has no KICK_MEMBERS bit.
        let plain_kp = Keypair::generate();
        members::register_member(&conn, &plain_kp.public_key(), "Plain").unwrap();
        members::assign_role(&conn, &plain_kp.public_key(), everyone_id).unwrap();
        // Add plain to the log_state as a real member so the content gate passes.
        {
            let mut guard = state.log_state.lock().unwrap();
            let ls = guard.as_mut().unwrap();
            // Manually insert via a second approval-free invite + join chain.
            let inv2 = Event::next(
                &owner_dev,
                owner_kp.public_key(),
                g.server_id(),
                Some(&inv),
                2,
                5,
                EP::InviteCreated {
                    code_hash: "testhash2".into(),
                    max_uses: 10,
                    expires_at: 9999999999,
                    requires_approval: false,
                },
            );
            ls.apply(&inv2).unwrap();
            let plain_dev = Keypair::generate();
            let plain_cert = DeviceCert::create(&plain_kp, &plain_dev.public_key(), 1);
            let plain_da = Event::next(
                &plain_dev,
                plain_kp.public_key(),
                g.server_id(),
                None,
                0,
                6,
                EP::DeviceAuthorized { cert: plain_cert },
            );
            ls.apply(&plain_da).unwrap();
            let plain_join = Event::next(
                &plain_dev,
                plain_kp.public_key(),
                g.server_id(),
                Some(&plain_da),
                1,
                7,
                EP::MemberJoined { member: plain_kp.public_key(), invite: inv2.hash() },
            );
            ls.apply(&plain_join).unwrap();
        }
        assert!(
            state.log_state.lock().unwrap().as_ref().unwrap().is_member(&plain_kp.public_key()),
            "plain member must be a log member for the content gate to pass"
        );

        let r2 = handle_request(
            &conn,
            &plain_kp.public_key(),
            false, // not owner
            ServerRequest::GetPendingMembers,
            "",
            &state,
        )
        .unwrap();
        match r2.response {
            ServerResponse::Error { reason } => {
                assert!(
                    reason.contains("KICK_MEMBERS"),
                    "expected KICK_MEMBERS error, got: {}",
                    reason
                );
            }
            other => panic!("expected Error for non-kick member, got {:?}", other),
        }
    }

    #[test]
    fn submit_event_broadcasts_membership_changed_on_member_event() {
        use crate::event_ingest;
        use farder_crypto::event_log::{DeviceCert, Event, EventPayload as EP, Genesis};
        use farder_crypto::event_log_state::LogState;

        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        let everyone_id = members::create_role(
            &conn, "@everyone", permissions::DEFAULT_EVERYONE, None, 0, true, false,
        ).unwrap();

        let owner_kp = Keypair::generate();
        let owner_dev = Keypair::generate();
        members::register_member(&conn, &owner_kp.public_key(), "Owner").unwrap();
        members::assign_role(&conn, &owner_kp.public_key(), everyone_id).unwrap();

        let g = Genesis {
            version: 1,
            name: "membership-broadcast-test".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        event_ingest::save_genesis(&conn, &g).unwrap();
        let mut ls = LogState::from_genesis(&g);

        // Owner DeviceAuthorized — apply directly to build log state.
        let owner_cert = DeviceCert::create(&owner_kp, &owner_dev.public_key(), 1);
        let owner_da = Event::next(
            &owner_dev,
            owner_kp.public_key(),
            g.server_id(),
            None,
            0,
            1,
            EP::DeviceAuthorized { cert: owner_cert },
        );
        ls.apply(&owner_da).unwrap();

        // Owner creates an instant-join invite (requires_approval: false).
        let inv = Event::next(
            &owner_dev,
            owner_kp.public_key(),
            g.server_id(),
            Some(&owner_da),
            1,
            2,
            EP::InviteCreated {
                code_hash: "joinhash".into(),
                max_uses: 10,
                expires_at: 9999999999,
                requires_approval: false,
            },
        );
        ls.apply(&inv).unwrap();

        // Alice registers her device.
        let alice_kp = Keypair::generate();
        let alice_dev = Keypair::generate();
        let alice_cert = DeviceCert::create(&alice_kp, &alice_dev.public_key(), 1);
        let alice_da = Event::next(
            &alice_dev,
            alice_kp.public_key(),
            g.server_id(),
            None,
            0,
            3,
            EP::DeviceAuthorized { cert: alice_cert },
        );
        ls.apply(&alice_da).unwrap();

        // Build Alice's MemberJoined event (not yet applied to ls — SubmitEvent will do it).
        let alice_join = Event::next(
            &alice_dev,
            alice_kp.public_key(),
            g.server_id(),
            Some(&alice_da),
            1,
            4,
            EP::MemberJoined { member: alice_kp.public_key(), invite: inv.hash() },
        );

        // Register alice in the legacy members table (needed for internal lookups).
        members::register_member(&conn, &alice_kp.public_key(), "Alice").unwrap();

        // Seed the live log_state with ls (before alice's join).
        *state.log_state.lock().unwrap() = Some(ls);

        // Submit the MemberJoined event through the real handler.
        let result = handle_request(
            &conn,
            &alice_kp.public_key(),
            false,
            ServerRequest::SubmitEvent { event: alice_join },
            "",
            &state,
        ).unwrap();

        assert!(
            matches!(result.response, ServerResponse::EventAccepted { .. }),
            "MemberJoined should be accepted, got {:?}",
            result.response,
        );
        assert!(
            result.events.iter().any(|b| matches!(&b.event, ServerEvent::MembershipChanged { .. })),
            "an accepted membership event must broadcast MembershipChanged",
        );
    }

    #[test]
    fn get_server_info_hides_structure_from_non_members() {
        use crate::event_ingest;
        use farder_crypto::event_log::Genesis;
        use farder_crypto::event_log_state::LogState;

        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        members::create_role(&conn, "@everyone", permissions::DEFAULT_EVERYONE, None, 0, true, false).unwrap();

        let owner_kp = Keypair::generate();
        let everyone_id: u64 = conn
            .query_row("SELECT id FROM roles WHERE name = '@everyone'", [], |row| {
                Ok(row.get::<_, i64>(0)? as u64)
            })
            .unwrap();
        members::register_member(&conn, &owner_kp.public_key(), "Owner").unwrap();
        members::assign_role(&conn, &owner_kp.public_key(), everyone_id).unwrap();

        let g = Genesis {
            version: 1,
            name: "mesh-test".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        event_ingest::save_genesis(&conn, &g).unwrap();
        *state.log_state.lock().unwrap() = Some(LogState::from_genesis(&g));
        *state.genesis.lock().unwrap() = Some(g);

        let owner = owner_kp.public_key();
        let stranger = farder_crypto::identity::Keypair::generate().public_key();

        // Non-member gets empty channels/categories/roles but still gets server_id.
        let r = handle_request(&conn, &stranger, false, ServerRequest::GetServerInfo, "", &state).unwrap();
        match r.response {
            ServerResponse::ServerInfo { channels, categories, roles, server_id, .. } => {
                assert!(channels.is_empty() && categories.is_empty() && roles.is_empty(),
                    "non-member gets no channel/role structure");
                assert!(server_id.is_some(), "but still learns server_id to join");
            }
            o => panic!("got {:?}", o),
        }

        // Owner (log member) still gets full ServerInfo response (not an Error).
        let r2 = handle_request(&conn, &owner, true, ServerRequest::GetServerInfo, "", &state).unwrap();
        match r2.response {
            ServerResponse::ServerInfo { .. } => {}
            o => panic!("owner should get ServerInfo, got {:?}", o),
        }
    }

    #[test]
    fn content_block_reason_helper_correctness() {
        use crate::event_ingest;
        use farder_crypto::event_log::Genesis;
        use farder_crypto::event_log_state::LogState;

        // --- Mesh server ---
        let state = Arc::new(ServerState::new_for_test().unwrap());
        let conn = state.db.lock().unwrap();

        members::create_role(&conn, "@everyone", permissions::DEFAULT_EVERYONE, None, 0, true, false).unwrap();

        let owner_kp = Keypair::generate();
        let g = Genesis {
            version: 1,
            name: "mesh-test".into(),
            owner: owner_kp.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        event_ingest::save_genesis(&conn, &g).unwrap();
        *state.log_state.lock().unwrap() = Some(LogState::from_genesis(&g));
        drop(conn);

        let owner_pk = owner_kp.public_key();
        let stranger_pk = Keypair::generate().public_key();

        // Owner is a log member — must not be blocked.
        assert!(
            content_block_reason(&state, &owner_pk).is_none(),
            "log member must not be blocked"
        );

        // Stranger is not a log member — must be blocked.
        let reason = content_block_reason(&state, &stranger_pk);
        assert!(reason.is_some(), "non-member must be blocked on a mesh server");
        assert!(
            reason.unwrap().contains("not a member"),
            "reason should indicate non-member"
        );

        // --- Legacy server (no log_state) ---
        let legacy_state = Arc::new(ServerState::new_for_test().unwrap());
        // log_state stays None
        assert!(
            content_block_reason(&legacy_state, &stranger_pk).is_none(),
            "legacy server (no log) must never block"
        );
    }

    #[test]
    fn add_bot_creates_member_and_remove_deletes_it() {
        let (conn, owner) = setup();
        let state = fake_state();

        // Owner can add a bot.
        let res = handle_request(
            &conn, &owner, true,
            ServerRequest::AddBot { coin_id: "bitcoin".into(), label: "BTC".into() },
            "", &state,
        ).unwrap();
        assert!(matches!(res.response, ServerResponse::Ok), "AddBot should return Ok");
        assert_eq!(res.events.len(), 1, "AddBot should broadcast MemberJoined");
        assert!(
            matches!(&res.events[0].event, ServerEvent::MemberJoined { display_name, .. } if display_name == "BTC"),
            "broadcast should be MemberJoined with bot label"
        );

        // Bots table and members table are both populated.
        let bots = crate::bots::list_bots(&conn).unwrap();
        assert_eq!(bots.len(), 1, "one bot should exist in bots table");
        let bot_pk = bots[0].public_key.clone();
        let all_members = members::list_members(&conn).unwrap();
        assert!(
            all_members.iter().any(|m| m.public_key == bot_pk && m.is_bot),
            "bot should appear in members with is_bot=true"
        );

        // Non-owner cannot add a bot.
        let stranger = farder_crypto::identity::Keypair::generate().public_key();
        let denied = handle_request(
            &conn, &stranger, false,
            ServerRequest::AddBot { coin_id: "ethereum".into(), label: "ETH".into() },
            "", &state,
        ).unwrap();
        assert!(
            !matches!(denied.response, ServerResponse::Ok),
            "non-owner AddBot must be denied"
        );

        // Owner can remove the bot.
        let res2 = handle_request(
            &conn, &owner, true,
            ServerRequest::RemoveBot { bot_public_key: bot_pk.clone() },
            "", &state,
        ).unwrap();
        assert!(matches!(res2.response, ServerResponse::Ok), "RemoveBot should return Ok");
        assert_eq!(res2.events.len(), 1, "RemoveBot should broadcast MemberLeft");
        assert!(
            matches!(&res2.events[0].event, ServerEvent::MemberLeft { public_key } if *public_key == bot_pk),
            "broadcast should be MemberLeft for the bot's key"
        );

        // Both tables cleaned up.
        assert!(crate::bots::list_bots(&conn).unwrap().is_empty(), "bots table should be empty");
        assert!(
            !members::list_members(&conn).unwrap().iter().any(|m| m.public_key == bot_pk),
            "bot should be gone from members table"
        );
    }

    #[test]
    fn alert_subscription_handler_roundtrip() {
        let (conn, owner) = setup();
        let state = fake_state();
        let non_owner = add_member(&conn, "User");

        // Add a bot first.
        handle_request(
            &conn, &owner, true,
            ServerRequest::AddBot { coin_id: "bitcoin".into(), label: "BTC".into() },
            "", &state,
        ).unwrap();
        let bots = crate::bots::list_bots(&conn).unwrap();
        let bot_pk = bots[0].public_key.clone();

        // Non-owner cannot add an alert.
        let denied = handle_request(
            &conn, &non_owner, false,
            ServerRequest::AddBotAlert { bot_public_key: bot_pk.clone(), metric: "price_usd".into(), comparator: "above".into(), threshold: 70000.0 },
            "", &state,
        ).unwrap();
        assert!(
            matches!(denied.response, ServerResponse::Error { .. }),
            "non-owner AddBotAlert must be denied"
        );

        // Invalid metric is rejected.
        let bad = handle_request(
            &conn, &owner, true,
            ServerRequest::AddBotAlert { bot_public_key: bot_pk.clone(), metric: "bogus".into(), comparator: "above".into(), threshold: 1.0 },
            "", &state,
        ).unwrap();
        assert!(matches!(bad.response, ServerResponse::Error { .. }), "invalid metric must be rejected");

        // Invalid comparator is rejected.
        let bad2 = handle_request(
            &conn, &owner, true,
            ServerRequest::AddBotAlert { bot_public_key: bot_pk.clone(), metric: "price_usd".into(), comparator: "nope".into(), threshold: 1.0 },
            "", &state,
        ).unwrap();
        assert!(matches!(bad2.response, ServerResponse::Error { .. }), "invalid comparator must be rejected");

        // Owner can add alerts.
        let r1 = handle_request(
            &conn, &owner, true,
            ServerRequest::AddBotAlert { bot_public_key: bot_pk.clone(), metric: "price_usd".into(), comparator: "above".into(), threshold: 70000.0 },
            "", &state,
        ).unwrap();
        assert!(matches!(r1.response, ServerResponse::Ok), "AddBotAlert should return Ok");

        handle_request(
            &conn, &owner, true,
            ServerRequest::AddBotAlert { bot_public_key: bot_pk.clone(), metric: "change_24h".into(), comparator: "below".into(), threshold: -5.0 },
            "", &state,
        ).unwrap();

        // ListBotAlerts returns both alerts.
        let list_res = handle_request(
            &conn, &owner, true,
            ServerRequest::ListBotAlerts { bot_public_key: bot_pk.clone() },
            "", &state,
        ).unwrap();
        match &list_res.response {
            ServerResponse::BotAlerts { alerts } => assert_eq!(alerts.len(), 2, "should list 2 alerts"),
            other => panic!("expected BotAlerts, got {:?}", other),
        }

        // Non-owner cannot list alerts.
        let denied_list = handle_request(
            &conn, &non_owner, false,
            ServerRequest::ListBotAlerts { bot_public_key: bot_pk.clone() },
            "", &state,
        ).unwrap();
        assert!(matches!(denied_list.response, ServerResponse::Error { .. }), "non-owner ListBotAlerts must be denied");

        // Any member can subscribe (subscriber is always the authenticated caller).
        let sub_res = handle_request(
            &conn, &non_owner, false,
            ServerRequest::SubscribeBot { bot_public_key: bot_pk.clone() },
            "", &state,
        ).unwrap();
        assert!(matches!(sub_res.response, ServerResponse::Ok), "SubscribeBot should return Ok");

        // ListMySubscriptions returns the bot.
        let subs_res = handle_request(
            &conn, &non_owner, false,
            ServerRequest::ListMySubscriptions,
            "", &state,
        ).unwrap();
        match &subs_res.response {
            ServerResponse::MySubscriptions { bot_public_keys } => {
                assert_eq!(bot_public_keys.len(), 1);
                assert_eq!(bot_public_keys[0], bot_pk);
            }
            other => panic!("expected MySubscriptions, got {:?}", other),
        }

        // Any member can unsubscribe.
        let unsub_res = handle_request(
            &conn, &non_owner, false,
            ServerRequest::UnsubscribeBot { bot_public_key: bot_pk.clone() },
            "", &state,
        ).unwrap();
        assert!(matches!(unsub_res.response, ServerResponse::Ok), "UnsubscribeBot should return Ok");
        let subs_after = handle_request(
            &conn, &non_owner, false,
            ServerRequest::ListMySubscriptions,
            "", &state,
        ).unwrap();
        match &subs_after.response {
            ServerResponse::MySubscriptions { bot_public_keys } => assert!(bot_public_keys.is_empty()),
            other => panic!("expected MySubscriptions, got {:?}", other),
        }

        // RemoveBotAlert (owner-gated): remove first alert.
        let alerts_before = crate::bots::list_alerts_for_bot(&conn, &bot_pk).unwrap();
        assert_eq!(alerts_before.len(), 2);
        let first_id = alerts_before[0].id;
        handle_request(
            &conn, &owner, true,
            ServerRequest::RemoveBotAlert { alert_id: first_id },
            "", &state,
        ).unwrap();
        let alerts_after = crate::bots::list_alerts_for_bot(&conn, &bot_pk).unwrap();
        assert_eq!(alerts_after.len(), 1, "should have 1 alert after RemoveBotAlert");

        // RemoveBot cascades: re-subscribe so we can verify cascade.
        handle_request(&conn, &non_owner, false, ServerRequest::SubscribeBot { bot_public_key: bot_pk.clone() }, "", &state).unwrap();
        handle_request(
            &conn, &owner, true,
            ServerRequest::RemoveBot { bot_public_key: bot_pk.clone() },
            "", &state,
        ).unwrap();
        assert!(crate::bots::list_alerts_for_bot(&conn, &bot_pk).unwrap().is_empty(), "alerts must be gone after RemoveBot");
        assert!(crate::bots::list_subscribers_for_bot(&conn, &bot_pk).unwrap().is_empty(), "subscriptions must be gone after RemoveBot");
    }

    #[test]
    fn webhook_crud_handler_arms() {
        let (conn, owner) = setup();
        let state = fake_state();
        let channel_id = make_channel(&conn);
        let non_owner = add_member(&conn, "NonOwner");

        // Non-owner must be denied.
        let denied = handle_request(
            &conn, &non_owner, false,
            ServerRequest::CreateWebhook { channel_id, name: "CI".into() },
            "", &state,
        ).unwrap();
        assert!(
            matches!(&denied.response, ServerResponse::Error { reason } if reason.contains("MANAGE_SERVER")),
            "non-owner CreateWebhook must be denied"
        );

        // Owner creates a webhook.
        let res = handle_request(
            &conn, &owner, true,
            ServerRequest::CreateWebhook { channel_id, name: "CI".into() },
            "", &state,
        ).unwrap();
        let (wh_id, _token) = match res.response {
            ServerResponse::WebhookToken { id, token, .. } => (id, token),
            other => panic!("expected WebhookToken, got {:?}", other),
        };

        // ListWebhooks returns it (no token).
        let list_res = handle_request(
            &conn, &owner, true,
            ServerRequest::ListWebhooks { channel_id },
            "", &state,
        ).unwrap();
        match list_res.response {
            ServerResponse::Webhooks { webhooks } => {
                assert_eq!(webhooks.len(), 1);
                assert_eq!(webhooks[0].id, wh_id);
                assert_eq!(webhooks[0].name, "CI");
            }
            other => panic!("expected Webhooks, got {:?}", other),
        }

        // RegenerateWebhookToken rotates the token.
        let regen_res = handle_request(
            &conn, &owner, true,
            ServerRequest::RegenerateWebhookToken { id: wh_id },
            "", &state,
        ).unwrap();
        assert!(
            matches!(regen_res.response, ServerResponse::WebhookToken { .. }),
            "RegenerateWebhookToken must return WebhookToken"
        );

        // Non-existent webhook returns an error.
        let bad_res = handle_request(
            &conn, &owner, true,
            ServerRequest::RegenerateWebhookToken { id: 9999 },
            "", &state,
        ).unwrap();
        assert!(
            matches!(&bad_res.response, ServerResponse::Error { reason } if reason.contains("not found")),
            "missing webhook must return error"
        );

        // DeleteWebhook removes it.
        let del_res = handle_request(
            &conn, &owner, true,
            ServerRequest::DeleteWebhook { id: wh_id },
            "", &state,
        ).unwrap();
        assert!(matches!(del_res.response, ServerResponse::Ok), "DeleteWebhook must return Ok");

        // ListWebhooks now empty.
        let list2 = handle_request(
            &conn, &owner, true,
            ServerRequest::ListWebhooks { channel_id },
            "", &state,
        ).unwrap();
        match list2.response {
            ServerResponse::Webhooks { webhooks } => assert!(webhooks.is_empty(), "list must be empty after delete"),
            other => panic!("expected Webhooks, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // RunCommand channel-authorization tests
    // Tests for check_run_command_channel_auth — the function called by the
    // RunCommand dispatch arm in connection.rs before any outbound work.
    // -----------------------------------------------------------------------

    /// A member who lacks SEND_MESSAGES on the target channel must be refused.
    /// Also verifies that no message is inserted (the check happens before insert).
    #[test]
    fn test_run_command_auth_no_send_messages() {
        let (conn, _owner_pk) = setup();
        let channel_id = make_channel(&conn);

        // Strip SEND_MESSAGES from @everyone.
        conn.execute(
            "UPDATE roles SET permissions = ?1 WHERE name = '@everyone' AND builtin = 1",
            rusqlite::params![permissions::VIEW_CHANNEL as i64 | permissions::READ_MESSAGES as i64],
        )
        .unwrap();

        let restricted = add_member(&conn, "ReadOnly");

        let result = check_run_command_channel_auth(&conn, &restricted, false, channel_id).unwrap();
        match result {
            Some(reason) => assert!(
                reason.contains("SEND_MESSAGES"),
                "expected SEND_MESSAGES denial, got: {reason}"
            ),
            None => panic!("expected auth denial, got Ok(None)"),
        }

        // Confirm nothing was inserted (auth check fires before any insert).
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages WHERE channel_id = ?1", [channel_id as i64], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "no message must be inserted when auth fails");
    }

    /// A timed-out member must be refused regardless of their channel permissions.
    #[test]
    fn test_run_command_auth_timed_out() {
        let (conn, _owner_pk) = setup();
        let channel_id = make_channel(&conn);

        let actor = add_member(&conn, "SpammerActual");
        // Apply a 60-second timeout.
        members::set_timeout(&conn, &actor, current_unix_ms() + 60_000, Some("testing")).unwrap();

        let result = check_run_command_channel_auth(&conn, &actor, false, channel_id).unwrap();
        match result {
            Some(reason) => assert!(
                reason.contains("timed out"),
                "expected timed-out denial, got: {reason}"
            ),
            None => panic!("expected timed-out denial, got Ok(None)"),
        }
    }

    /// Owner is never blocked by the permission check (owners get ALL_PERMISSIONS).
    #[test]
    fn test_run_command_auth_owner_allowed() {
        let (conn, owner_pk) = setup();
        let channel_id = make_channel(&conn);

        let result = check_run_command_channel_auth(&conn, &owner_pk, true, channel_id).unwrap();
        assert!(result.is_none(), "owner must pass auth, got: {result:?}");
    }
}
