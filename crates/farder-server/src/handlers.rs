use crate::{
    channels,
    events::{BroadcastEvent, EventTarget},
    invites, members, messages, permissions,
};
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::*;
use rusqlite::Connection;

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
// Main dispatch
// ---------------------------------------------------------------------------

pub fn handle_request(
    conn: &Connection,
    member: &PublicKey,
    is_owner: bool,
    request: ServerRequest,
    storage_dir: &str,
) -> Result<HandleResult> {
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
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::ChannelCreated {
                    channel: channel.clone(),
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
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
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::ChannelUpdated {
                    channel: channel.clone(),
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::DeleteChannel { channel_id } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::MANAGE_CHANNEL) {
                return err("missing MANAGE_CHANNEL permission");
            }
            channels::soft_delete_channel(conn, channel_id)?;
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::ChannelDeleted { channel_id },
            };
            ok_with(ServerResponse::Ok, vec![event])
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
            )?;
            let role = members::get_role(conn, role_id)?.unwrap();
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::RoleCreated { role: role.clone() },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::UpdateRole {
            role_id,
            name,
            permissions: perms_bits,
            color,
            position,
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
            )?;
            let role = members::get_role(conn, role_id)?.unwrap();
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::RoleUpdated { role: role.clone() },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::DeleteRole { role_id } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_ROLES, "MANAGE_ROLES")? {
                return Ok(denied);
            }
            let target_role = members::get_role(conn, role_id)?.ok_or_else(|| anyhow::anyhow!("role not found"))?;
            if let Some(denied) = require_role_hierarchy(conn, member, is_owner, target_role.position)? {
                return Ok(denied);
            }
            members::delete_role(conn, role_id)?;
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::RoleDeleted { role_id },
            };
            ok_with(ServerResponse::Ok, vec![event])
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
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::PermissionsChanged,
            };
            ok_with(ServerResponse::Ok, vec![event])
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
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::PermissionsChanged,
            };
            ok_with(ServerResponse::Ok, vec![event])
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
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberLeft {
                    public_key: member_key,
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::BanMember { member_key, reason } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::BAN_MEMBERS, "BAN_MEMBERS")? {
                return Ok(denied);
            }
            if let Some(denied) = require_member_hierarchy(conn, member, is_owner, &member_key)? {
                return Ok(denied);
            }
            members::ban_member(conn, &member_key, reason.as_deref())?;
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberBanned {
                    public_key: member_key,
                    reason,
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        // ----------------------------------------------------------------
        // Unban / List banned
        // ----------------------------------------------------------------
        ServerRequest::UnbanMember { member_key } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::BAN_MEMBERS, "BAN_MEMBERS")? {
                return Ok(denied);
            }
            members::unban_member(conn, &member_key)?;
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberUnbanned { public_key: member_key.clone() },
            };
            ok_with(ServerResponse::Ok, vec![event])
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
            let channels_list = channels::list_channels(conn)?;
            let categories_list = channels::list_categories(conn)?;
            let roles_list = members::list_roles(conn)?;
            ok(ServerResponse::ServerInfo {
                name: String::new(), // patched by connection handler
                member_count,
                channels: channels_list,
                categories: categories_list,
                roles: roles_list,
            })
        }

        ServerRequest::GetMembers => {
            let all_members = members::list_members(conn)?;
            let mut member_infos: Vec<MemberInfo> = Vec::new();
            for m in all_members {
                let role_ids = members::get_member_role_ids(conn, &m.public_key)?;
                member_infos.push(MemberInfo {
                    public_key: m.public_key,
                    display_name: m.display_name,
                    joined_at: m.joined_at,
                    role_ids,
                });
            }
            ok(ServerResponse::Members {
                members: member_infos,
            })
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
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::PermissionsChanged,
            };
            ok_with(ServerResponse::Ok, vec![event])
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
            let msg = messages::get_message(conn, message_id, member)?
                .ok_or_else(|| anyhow::anyhow!("message not found"))?;
            let perms = resolve_member_perms(conn, member, msg.channel_id, is_owner)?;
            if !permissions::has(perms, permissions::READ_MESSAGES) {
                return err("missing READ_MESSAGES permission");
            }
            crate::reactions::add_reaction(conn, message_id, member, &emoji, file_id)?;
            ok_with(ServerResponse::Ok, vec![BroadcastEvent {
                target: EventTarget::Subscribers(msg.channel_id),
                event: ServerEvent::ReactionAdded {
                    message_id, channel_id: msg.channel_id, emoji, public_key: member.clone(), file_id,
                },
            }])
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

            // Build MemberInfo for participant.
            let target_record = members::get_member(conn, &target_key)?
                .ok_or_else(|| anyhow::anyhow!("target member disappeared"))?;
            let role_ids = members::get_member_role_ids(conn, &target_key)?;
            let participant = MemberInfo {
                public_key: target_key.clone(),
                display_name: target_record.display_name,
                joined_at: target_record.joined_at,
                role_ids,
            };

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
                let participant = MemberInfo {
                    public_key: other_key,
                    display_name: other_record.display_name,
                    joined_at: other_record.joined_at,
                    role_ids,
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

        // ----------------------------------------------------------------
        // Voice (Phase 4)
        // ----------------------------------------------------------------
        ServerRequest::JoinVoice { channel_id } => {
            let channel = channels::get_channel(conn, channel_id)?
                .ok_or_else(|| anyhow::anyhow!("channel not found"))?;
            if channel.channel_type != ChannelType::Voice {
                return err("not a voice channel");
            }
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::CONNECT) {
                return err("missing CONNECT permission");
            }
            // Leave any existing voice channel first
            let left_channels = channels::leave_all_voice(conn, member)?;
            let mut events: Vec<BroadcastEvent> = Vec::new();
            for left_ch in left_channels {
                events.push(BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::VoiceLeft { channel_id: left_ch, public_key: member.clone() },
                });
            }
            // Join the new channel
            channels::join_voice(conn, channel_id, member)?;
            let display_name = members::get_member(conn, member)?
                .map(|m| m.display_name).unwrap_or_default();
            events.push(BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::VoiceJoined { channel_id, public_key: member.clone(), display_name },
            });
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::LeaveVoice { channel_id } => {
            channels::leave_voice(conn, channel_id, member)?;
            ok_with(ServerResponse::Ok, vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::VoiceLeft { channel_id, public_key: member.clone() },
            }])
        }

        ServerRequest::GetVoiceState { channel_id } => {
            let participants = channels::get_voice_participants(conn, channel_id)?;
            let mut voice_members = Vec::new();
            for (pk, joined_at) in participants {
                let name = members::get_member(conn, &pk)?
                    .map(|m| m.display_name).unwrap_or_default();
                voice_members.push(VoiceMember { public_key: pk, display_name: name, joined_at });
            }
            ok(ServerResponse::VoiceStateResp { participants: voice_members })
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
            },
            "",
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
        let mod_role_id = members::create_role(&conn, "Mod", permissions::MANAGE_ROLES | permissions::DEFAULT_EVERYONE, None, 2, false).unwrap();
        let moderator = add_member(&conn, "Moderator");
        members::assign_role(&conn, &moderator, mod_role_id).unwrap();

        // Mod tries to create a role at position 3 (above their position 2) — should fail
        let result = handle_request(&conn, &moderator, false, ServerRequest::CreateRole {
            name: "SuperAdmin".to_string(),
            permissions: permissions::ALL_PERMISSIONS,
            color: None,
            position: Some(3),
        }, "").unwrap();
        match result.response {
            ServerResponse::Error { .. } => {}
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn test_cannot_kick_member_with_higher_role() {
        let (conn, _owner) = setup();
        // Create admin role at position 3, mod role at position 2
        let admin_role = members::create_role(&conn, "Admin", permissions::ALL_PERMISSIONS, None, 3, false).unwrap();
        let mod_role = members::create_role(&conn, "Mod", permissions::KICK_MEMBERS | permissions::DEFAULT_EVERYONE, None, 2, false).unwrap();

        let admin = add_member(&conn, "Admin");
        members::assign_role(&conn, &admin, admin_role).unwrap();

        let moderator = add_member(&conn, "Moderator");
        members::assign_role(&conn, &moderator, mod_role).unwrap();

        // Mod tries to kick Admin — should fail (admin position 3 > mod position 2)
        let result = handle_request(&conn, &moderator, false, ServerRequest::KickMember {
            member_key: admin.clone(),
        }, "").unwrap();
        match result.response {
            ServerResponse::Error { .. } => {}
            other => panic!("expected Error, got {:?}", other),
        }

        // Mod can kick a regular member (position 0, below mod's 2)
        let regular = add_member(&conn, "Regular");
        let result = handle_request(&conn, &moderator, false, ServerRequest::KickMember {
            member_key: regular.clone(),
        }, "").unwrap();
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
        }, "").unwrap();
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
        }, "").unwrap();
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
        }, "").unwrap();
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
        }, "").unwrap();
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
        }, "").unwrap();
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
        }, "").unwrap();
        match result.response { ServerResponse::Ok => {} other => panic!("expected Ok, got {:?}", other) }
    }

    #[test]
    fn test_handle_request_deletion() {
        let (conn, _owner_pk) = setup();
        let member = add_member(&conn, "Alice");

        let result = handle_request(&conn, &member, false, ServerRequest::RequestDeletion, "").unwrap();
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

        let result = handle_request(&conn, &owner_pk, true, ServerRequest::RequestDeletion, "").unwrap();
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
        handle_request(&conn, &member, false, ServerRequest::RequestDeletion, "").unwrap();

        // Cancel deletion
        let result = handle_request(&conn, &member, false, ServerRequest::CancelDeletion, "").unwrap();
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
        let result = handle_request(&conn, &member, false, ServerRequest::GetDeletionStatus, "").unwrap();
        match result.response {
            ServerResponse::DeletionStatusResp { status } => {
                assert!(!status.pending);
                assert!(status.requested_at.is_none());
                assert!(status.expires_at.is_none());
            }
            other => panic!("expected DeletionStatusResp, got {:?}", other),
        }

        // After requesting deletion: pending
        handle_request(&conn, &member, false, ServerRequest::RequestDeletion, "").unwrap();
        let result = handle_request(&conn, &member, false, ServerRequest::GetDeletionStatus, "").unwrap();
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
        )
        .unwrap();
        let r2 = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::OpenDm { target_key: alice.clone() },
            "",
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
        handle_request(&conn, &owner_pk, true, ServerRequest::OpenDm { target_key: alice }, "").unwrap();
        handle_request(&conn, &owner_pk, true, ServerRequest::OpenDm { target_key: bob }, "").unwrap();

        let result = handle_request(&conn, &owner_pk, true, ServerRequest::ListDms, "").unwrap();
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
        )
        .unwrap();
        let dm_channel_id = match open_result.response {
            ServerResponse::DmOpened { channel, .. } => channel.id,
            other => panic!("expected DmOpened, got {:?}", other),
        };

        // Block alice.
        handle_request(&conn, &owner_pk, true, ServerRequest::BlockUser { target_key: alice.clone() }, "").unwrap();

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
        )
        .unwrap();

        // Now unban.
        let result = handle_request(
            &conn,
            &owner_pk,
            true,
            ServerRequest::UnbanMember { member_key: victim.clone() },
            "",
        )
        .unwrap();

        assert!(matches!(result.response, ServerResponse::Ok));
        assert_eq!(result.events.len(), 1);
        assert!(matches!(result.events[0].event, ServerEvent::MemberUnbanned { .. }));
        assert!(crate::members::list_banned(&conn).unwrap().is_empty());
    }
}
