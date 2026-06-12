import { useApp } from "../context/ServerContext";
import { parseInviteLink } from "../lib/invite";
import { useInvitePreview } from "../hooks/useInvitePreview";

interface InviteEmbedProps {
  link: string;
}

export default function InviteEmbed({ link }: InviteEmbedProps) {
  const { dispatch } = useApp();
  const address = parseInviteLink(link).address ?? "";
  const relayed = /^farder:\/\/relayd?\//i.test(address);
  const preview = useInvitePreview(link);

  return (
    <div className="invite-embed">
      <div className="invite-embed-title">
        {preview.status === "ok" && preview.serverName ? preview.serverName : "Server invite"}
      </div>
      {preview.status === "ok" && (
        <div className="invite-embed-counts">
          {preview.memberCount ?? 0} members &middot; {preview.onlineCount ?? 0} online
        </div>
      )}
      {preview.status === "loading" && (
        <div className="invite-embed-state">Loading preview&hellip;</div>
      )}
      {preview.status === "invalid" && (
        <div className="invite-embed-state">Invite invalid or expired</div>
      )}
      {preview.status === "unavailable" && (
        <div className="invite-embed-state">Preview unavailable</div>
      )}
      <div className={`join-relay-note ${relayed ? "relayed" : "direct"}`}>
        <span className="join-relay-badge">{relayed ? "RELAYED" : "DIRECT"}</span>
        <span>{relayed ? "Your IP stays hidden from the host." : "The host can see your IP address."}</span>
      </div>
      <button
        className="xp-button invite-embed-join"
        onClick={() => dispatch({ type: "OPEN_JOIN_CONFIRM", link })}
        disabled={preview.status === "invalid"}
      >
        Join
      </button>
    </div>
  );
}
