use crate::{
    channels, db,
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
}

// ---------------------------------------------------------------------------
// Helper constructors
// ---------------------------------------------------------------------------

fn ok(response: ServerResponse) -> Result<HandleResult> {
    Ok(HandleResult {
        response,
        events: vec![],
    })
}

fn ok_with(response: ServerResponse, events: Vec<BroadcastEvent>) -> Result<HandleResult> {
    Ok(HandleResult { response, events })
}

fn err(reason: &str) -> Result<HandleResult> {
    Ok(HandleResult {
        response: ServerResponse::Error {
            reason: reason.to_string(),
        },
        events: vec![],
    })
}

// ---------------------------------------------------------------------------
// Permission resolution helper
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Main dispatch
// ---------------------------------------------------------------------------

pub fn handle_request(
    conn: &Connection,
    member: &PublicKey,
    is_owner: bool,
    request: ServerRequest,
) -> Result<HandleResult> {
    match request {
        // ----------------------------------------------------------------
        // Messaging
        // ----------------------------------------------------------------
        ServerRequest::SendMessage {
            channel_id,
            content,
            reply_to,
        } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::SEND_MESSAGES) {
                return err("missing SEND_MESSAGES permission");
            }
            let id = messages::insert_message(conn, channel_id, member, &content, reply_to)?;
            let msg = match messages::get_message(conn, id)? {
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
            let msg = match messages::get_message(conn, message_id)? {
                Some(m) => m,
                None => return err("message not found"),
            };
            if msg.author != *member {
                return err("can only edit own messages");
            }
            messages::edit_message(conn, message_id, &new_content)?;
            let updated = messages::get_message(conn, message_id)?.unwrap();
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
            let msg = match messages::get_message(conn, message_id)? {
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

            messages::delete_message(conn, message_id)?;
            let event = BroadcastEvent {
                target: EventTarget::Subscribers(channel_id),
                event: ServerEvent::MessageDeleted {
                    message_id,
                    channel_id,
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
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
            let msgs = messages::fetch_history(conn, channel_id, before_id, limit)?;
            ok(ServerResponse::History { messages: msgs })
        }

        ServerRequest::PinMessage { message_id } => {
            let msg = match messages::get_message(conn, message_id)? {
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
            let msg = match messages::get_message(conn, message_id)? {
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
            if let Some(cid) = channel_id {
                let perms = resolve_member_perms(conn, member, cid, is_owner)?;
                if !permissions::has(perms, permissions::READ_MESSAGES) {
                    return err("missing READ_MESSAGES permission");
                }
            }
            let msgs = messages::search_messages(conn, &query, channel_id, limit)?;
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
            if !is_owner {
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
                if !permissions::has(base, permissions::MANAGE_CHANNEL) {
                    return err("missing MANAGE_CHANNEL permission");
                }
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
            if !is_owner {
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
                if !permissions::has(base, permissions::MANAGE_SERVER) {
                    return err("missing MANAGE_SERVER permission");
                }
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
            if !is_owner {
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
                if !permissions::has(base, permissions::MANAGE_SERVER) {
                    return err("missing MANAGE_SERVER permission");
                }
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
            if !is_owner {
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
                if !permissions::has(base, permissions::MANAGE_SERVER) {
                    return err("missing MANAGE_SERVER permission");
                }
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
            if !is_owner {
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
                if !permissions::has(base, permissions::MANAGE_ROLES) {
                    return err("missing MANAGE_ROLES permission");
                }
            }
            let pos = position.unwrap_or(0);
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
            if !is_owner {
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
                if !permissions::has(base, permissions::MANAGE_ROLES) {
                    return err("missing MANAGE_ROLES permission");
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
            if !is_owner {
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
                if !permissions::has(base, permissions::MANAGE_ROLES) {
                    return err("missing MANAGE_ROLES permission");
                }
            }
            members::delete_role(conn, role_id)?;
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::RoleDeleted { role_id },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::AssignRole { member_key, role_id } => {
            if !is_owner {
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
                if !permissions::has(base, permissions::MANAGE_ROLES) {
                    return err("missing MANAGE_ROLES permission");
                }
            }
            members::assign_role(conn, &member_key, role_id)?;
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::PermissionsChanged,
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::RemoveRole { member_key, role_id } => {
            if !is_owner {
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
                if !permissions::has(base, permissions::MANAGE_ROLES) {
                    return err("missing MANAGE_ROLES permission");
                }
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
            if !is_owner {
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
                if !permissions::has(base, permissions::KICK_MEMBERS) {
                    return err("missing KICK_MEMBERS permission");
                }
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

        ServerRequest::BanMember { member_key } => {
            if !is_owner {
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
                if !permissions::has(base, permissions::BAN_MEMBERS) {
                    return err("missing BAN_MEMBERS permission");
                }
            }
            members::ban_member(conn, &member_key)?;
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberBanned {
                    public_key: member_key,
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        // ----------------------------------------------------------------
        // Invites
        // ----------------------------------------------------------------
        ServerRequest::CreateInvite {
            max_uses,
            expires_in_secs,
            target_channel,
        } => {
            if !is_owner {
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
                if !permissions::has(base, permissions::CREATE_INVITES) {
                    return err("missing CREATE_INVITES permission");
                }
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
            if !is_owner {
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
                if !permissions::has(base, permissions::MANAGE_SERVER) {
                    return err("missing MANAGE_SERVER permission");
                }
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
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
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
            },
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
            },
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
            },
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
        )
        .unwrap();

        assert!(
            matches!(edit_result.response, ServerResponse::Ok),
            "expected Ok"
        );

        // Verify content changed.
        let msg = messages::get_message(&conn, msg_id).unwrap().unwrap();
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
            },
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
}
