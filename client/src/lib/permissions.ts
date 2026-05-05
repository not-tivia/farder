import type { MemberInfo, RoleInfo } from "./types";
import { publicKeyToString } from "./types";

// Permission flags must match crates/farder-server/src/permissions.rs
export const PERMISSIONS = {
  CREATE_INSTANT_INVITE: 1n << 0n,
  MANAGE_MESSAGES: 1n << 3n,
  MANAGE_CHANNEL: 1n << 7n,
  MANAGE_ROLES: 1n << 8n,
  MANAGE_SERVER: 1n << 9n,
  KICK_MEMBERS: 1n << 10n,
  BAN_MEMBERS: 1n << 11n,
} as const;

/** Compute the bitwise OR of all role permissions for the given member. */
export function resolveMemberPermissions(member: MemberInfo, roles: RoleInfo[]): bigint {
  if (member.role_ids.length === 0) return 0n;
  let bits = 0n;
  for (const roleId of member.role_ids) {
    const role = roles.find((r) => r.id === roleId);
    if (!role) continue;
    bits |= BigInt(role.permissions);
  }
  return bits;
}

export function hasPermission(bits: bigint, perm: bigint): boolean {
  return (bits & perm) === perm;
}

/** Find the actor's MemberInfo + their resolved permissions in one shot. */
export function getActorPermissions(
  members: MemberInfo[],
  roles: RoleInfo[],
  ownPk: string,
): { member: MemberInfo | null; bits: bigint } {
  const member = members.find((m) => publicKeyToString(m.public_key) === ownPk) ?? null;
  if (!member) return { member: null, bits: 0n };
  return { member, bits: resolveMemberPermissions(member, roles) };
}
