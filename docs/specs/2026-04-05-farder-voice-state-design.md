# Farder: Voice State (Phase 1) — Design Spec

**Date:** 2026-04-05
**Status:** Draft
**Depends On:** Phase 2 (Servers & Text), Client v1

## Goal

Add voice channels where users can join and leave, with real-time participant tracking. No audio yet — this phase establishes the voice state infrastructure that audio streaming will build on.

## Server Architecture

### Channel Type

`ChannelType::Voice` added to the existing enum. Voice channels are created via server settings like text channels, placed in categories, and follow the same permission model.

### Voice State Table

**`voice_state`**

| Column | Type | Description |
|--------|------|-------------|
| channel_id | INTEGER NOT NULL | FK to channels.id |
| user_key | BLOB NOT NULL | Public key of the user |
| joined_at | INTEGER NOT NULL | Unix timestamp |
| PRIMARY KEY | (channel_id, user_key) | |

Constraint: a user can only be in one voice channel at a time across the server. Joining a new channel auto-removes from the old one.

### Protocol Changes

**New requests:**
```
JoinVoice { channel_id: u64 }
LeaveVoice { channel_id: u64 }
GetVoiceState { channel_id: u64 }
```

**New responses:**
```
VoiceState { participants: Vec<VoiceMember> }
```

Where:
```
VoiceMember {
    public_key: PublicKey,
    display_name: String,
    joined_at: u64,
}
```

**New events:**
```
VoiceJoined { channel_id: u64, public_key: PublicKey, display_name: String }
VoiceLeft { channel_id: u64, public_key: PublicKey }
```

Events are broadcast to ALL connected members (not just subscribers) so the sidebar can show participant counts.

### Request Handling

**JoinVoice:**
1. Verify user has `CONNECT` permission (bit 4) for the channel
2. Verify channel type is Voice
3. Remove user from any other voice channel on this server (auto-leave)
4. Insert into `voice_state`
5. Broadcast `VoiceJoined` to all connected members
6. Return `Ok`

**LeaveVoice:**
1. Remove from `voice_state`
2. Broadcast `VoiceLeft` to all connected members
3. Return `Ok`

**GetVoiceState:**
1. Query `voice_state` for the channel
2. Look up display names for each participant
3. Return `VoiceState { participants }`

**Disconnect cleanup:**
When a client disconnects, remove them from any voice channel they're in and broadcast `VoiceLeft`.

## Client Architecture

### Sidebar Display

Voice channels appear in the sidebar alongside text channels with a different icon (`~` instead of `#`). Below each voice channel, if there are connected users, show them as an indented sub-list:

```
GENERAL
  # general
  # chat
  ~ Voice Chat
    * Alice
    * Bob
  ~ AFK
```

Clicking a voice channel name joins it (or leaves if already in). No separate join/leave buttons needed — the click toggles.

### Voice Status Bar

At the bottom of the sidebar, above the user footer, show a "Voice Connected" bar when in a voice channel:

```
[~] Voice Chat — Connected    [X]
```

The `[X]` button disconnects. This bar is always visible while connected to voice, even when browsing other channels.

### State Management

Add to `PerServerState`:
```typescript
voiceStates: Record<number, VoiceMember[]>; // channelId -> participants
currentVoiceChannelId: number | null;
```

### Channel Participant Count

For the sidebar, voice channels show the number of connected users next to the name: `~ Voice Chat (2)`

## Permissions

- `CONNECT` (bit 4) — required to join a voice channel
- Voice channels are visible to anyone with `VIEW_CHANNEL`
- No speaking permissions enforced yet (no audio in Phase 1)

## What's NOT in Phase 1

- Audio capture, encoding, streaming, or playback
- Mute/deafen controls
- Push-to-talk or voice activity detection
- Speaking indicators
- Screen sharing
- Voice channel user limits
