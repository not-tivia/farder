import type { Dispatch } from "react";
import type { AppAction } from "../context/ServerContext";
import type { ServerInfoV2 } from "./types";

/**
 * After a v1 `SERVER_ADDED`, re-fetch the server info through the v2 surface so
 * each channel's `class` is populated. The v1 `ConnectResult` returned by
 * `connect_server` / `create_local_server` carries channels with NO class, and
 * the `SERVER_ADDED` reducer defaults them to "Plaintext" — so an E2EE channel
 * that already existed before this connect (restart, or joining an existing
 * server) would render as plaintext without this refresh.
 *
 * Non-fatal by design: the v2 fetch requires the connection to have negotiated
 * (done inside `connect_server` / `create_local_server` before they return), but
 * a failed fetch (older sidecar, transient transport error) is swallowed so it
 * can never break the connect flow — the channels stay on the plaintext default
 * until the next v2 refresh (e.g. a server switch in `ServerStrip`).
 */
export function refreshServerClasses(
  serverId: string,
  dispatch: Dispatch<AppAction>,
  api: { getServerInfoV2: (serverId: string) => Promise<ServerInfoV2> },
): void {
  api
    .getServerInfoV2(serverId)
    .then((info) => dispatch({ type: "SERVER_REFRESHED", serverId, payload: info }))
    .catch(() => {});
}
