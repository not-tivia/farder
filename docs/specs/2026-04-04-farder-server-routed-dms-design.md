# Farder: Server-Routed Direct Messages — Design Spec

**Date:** 2026-04-04
**Status:** Draft
**Depends On:** Phase 2 (Servers & Text), Client v1

## Goal

Add direct messaging between users on the same server. DMs are private channels routed through the server. Plaintext for now — E2EE will be added before public release (see project memory note).

## Server Architecture

### DM Channels

A DM is a channel with `channel_type = "dm"`. It reuses the existing message, attachment, reaction, and thread infrastructure. DM channels are:
- Auto-created when a user initiates a DM with another user
- Invisible in `GetServerInfo` channel lists (fetched via `ListDms`)
- Not associated with any category
- Limited to exactly two participants

### New Tables

**`dm_participants`**

| Column | Type | Description |
|--------|------|-------------|
| channel_id | INTEGER NOT NULL | FK to channels.id |
| user_key | BLOB NOT NULL | Public key of participant |
| PRIMARY KEY | (channel_id, user_key) | |

**`blocked_users`**

| Column | Type | Description |
|--------|------|-------------|
| blocker_key | BLOB NOT NULL | Public key of the user who blocked |
| blocked_key | BLOB NOT NULL | Public key of the blocked user |
| blocked_at | INTEGER NOT NULL | Unix timestamp |
| PRIMARY KEY | (blocker_key, blocked_key) | |

### New Protocol Types

**Requests:**
```
OpenDm { target_key: PublicKey }     — find or create DM channel, returns DmInfo
ListDms                               — returns all DM channels for the requester
BlockUser { target_key: PublicKey }   — block a user (prevents DMs)
UnblockUser { target_key: PublicKey } — unblock a user
```

**Responses:**
```
DmOpened { channel: ChannelInfo, participant: MemberInfo }
DmList { dms: Vec<DmEntry> }
```

Where `DmEntry`:
```
DmEntry {
    channel: ChannelInfo,
    participant: MemberInfo,       // the OTHER user
    last_message: Option<MessageInfo>,  // for sorting by recent activity
}
```

**Events:**
```
DmCreated { channel: ChannelInfo, participant: MemberInfo }  — sent to both participants
```

### Request Handling

**OpenDm:**
1. Verify both users are members of the server
2. Check neither has blocked the other
3. Look for existing DM channel between these two users (query dm_participants)
4. If found: return it
5. If not: create a new channel (type="dm", name="dm"), insert two dm_participants rows, return it

**ListDms:**
1. Query dm_participants for all channels where the requester is a participant
2. For each, load the other participant's MemberInfo and the last message
3. Sort by last message timestamp (most recent first)
4. Return the list

**SendMessage in DM channel:**
1. Verify the sender is a participant (check dm_participants)
2. Verify neither user has blocked the other
3. Proceed with normal message insertion
4. Push NewMessage event to the other participant (if connected and subscribed)

**BlockUser:**
1. Insert into blocked_users
2. Return Ok

**UnblockUser:**
1. Delete from blocked_users
2. Return Ok

### DM Channel Visibility

- `list_channels` already excludes threads and will also exclude DMs (`channel_type != 'dm'`)
- DMs are only accessible via `ListDms` or by knowing the channel ID
- Permission checks on DM channels bypass the normal role-based system — both participants always have full access

## Client Architecture

### Sidebar — DM Section

Below the server channels section, add a "DIRECT MESSAGES" header with a list of active DM conversations. Each entry shows:
- Other user's display name
- Last message preview (truncated)
- Unread indicator (bold + dot, same as channels)

Clicking a DM entry loads the DM in the main chat panel (same as clicking a channel).

### Opening a DM

From the profile popup (click a username), add a "Message" button. Clicking it:
1. Calls `OpenDm { target_key }` to find/create the DM channel
2. Subscribes to the DM channel
3. Fetches history
4. Switches the main panel to show the DM

### Pop-Out DM Panel

A button in the DM chat header ("Pop Out" or a side-panel icon) opens the DM in a **narrow right panel** alongside the main chat. This lets users:
- View a server channel in the main panel
- Chat in a DM in the side panel
- Both receive real-time events

The side panel is ~300px wide, slides in from the right, and has its own message list + input. It can be closed with an X button.

### Block/Unblock

In the profile popup, add:
- "Block User" — calls BlockUser, hides the DM conversation
- "Unblock User" (if already blocked) — calls UnblockUser

## What's NOT Included

- E2EE for DMs (CRITICAL TODO — must add before public release)
- Group DMs (1:1 only)
- Cross-server DMs (requires relay infrastructure)
- DM notifications/sounds
- Read receipts
- Typing indicators in DMs (could reuse existing typing system)
- File uploads in DMs (reuses existing attachment system, should just work)
