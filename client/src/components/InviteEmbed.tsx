import { useApp } from "../context/ServerContext";
import { parseInviteLink } from "../lib/invite";

interface InviteEmbedProps {
  link: string;
}

export default function InviteEmbed({ link }: InviteEmbedProps) {
  const { dispatch } = useApp();
  const address = parseInviteLink(link).address ?? "";
  const relayed = /^farder:\/\/relayd?\//i.test(address);

  return (
    <div className="invite-embed">
      <div className="invite-embed-title">Server invite</div>
      <div className={`join-relay-note ${relayed ? "relayed" : "direct"}`}>
        <span className="join-relay-badge">{relayed ? "RELAYED" : "DIRECT"}</span>
        <span>{relayed ? "Your IP stays hidden from the host." : "The host can see your IP address."}</span>
      </div>
      <button
        className="xp-button invite-embed-join"
        onClick={() => dispatch({ type: "OPEN_JOIN_CONFIRM", link })}
      >
        Join
      </button>
    </div>
  );
}
